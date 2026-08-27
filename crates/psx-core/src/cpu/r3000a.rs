//! Interpretador do MIPS R3000A.
//!
//! Referência: PSX-SPX — "CPU Specifications", "CPU Opcode Encoding",
//! "CPU Load/Store/Multiply/Divide Timings".
//!
//! Quirks implementados aqui, todos observáveis por software:
//!
//! - **Load delay slot**: o resultado de um load só fica visível na instrução
//!   *seguinte*. Modelado com dois bancos de registradores (`regs` visível e
//!   `out_regs` sendo escrito) mais um slot de load pendente.
//! - **Branch delay slot**: a instrução após um branch sempre executa. Uma
//!   exceção dentro do delay slot grava `EPC` apontando para o *branch* e liga
//!   `Cause.BD`.
//! - **`ADD`/`ADDI`/`SUB` fazem trap em overflow**; `ADDU`/`SUBU` não.
//! - **`DIV` por zero não faz trap** — produz valores definidos.
//! - **`LWL`/`LWR`/`SWL`/`SWR`** operam sobre a palavra alinhada, sem exceção
//!   de alinhamento, e a forma `LWL`+`LWR` encadeada lê o registrador ainda
//!   dentro do load delay.
//! - **Latência de `HI`/`LO`**: `MFHI`/`MFLO` travam a CPU até `MULT`/`DIV`
//!   terminarem.

use crate::bus::Bus;
use crate::cpu::cop0::{Cop0, Exception};
use crate::cpu::instruction::Instruction;
use crate::gte::Gte;

/// Vetor de reset do R3000A: início da BIOS em KSEG1.
pub const RESET_VECTOR: u32 = 0xBFC0_0000;

/// Estado completo da CPU.
#[derive(Clone)]
pub struct Cpu {
    /// Endereço da próxima instrução a buscar.
    pc: u32,
    /// Endereço da instrução seguinte a essa (implementa o branch delay slot).
    next_pc: u32,
    /// Endereço da instrução sendo executada agora (vira `EPC` numa exceção).
    current_pc: u32,

    /// Banco visível de registradores gerais.
    regs: [u32; 32],
    /// Banco sendo escrito pela instrução atual (load delay slot).
    out_regs: [u32; 32],
    /// Load pendente: `(registrador, valor)`, aplicado antes da próxima instrução.
    load: (usize, u32),

    hi: u32,
    lo: u32,
    /// Ciclo em que `HI`/`LO` ficam prontos após `MULT`/`DIV`.
    hi_lo_ready_at: u64,

    /// A instrução atual desviou o fluxo (a próxima está num delay slot).
    branch_taken: bool,
    /// A instrução atual está num delay slot.
    in_delay_slot: bool,

    pub cop0: Cop0,
    pub gte: Gte,

    /// Ciclos executados desde o reset.
    cycles: u64,
    /// Ciclos extras cobrados pela instrução atual (stalls).
    stall: u32,
}

impl Cpu {
    pub fn new() -> Self {
        let mut cpu = Self {
            pc: 0,
            next_pc: 0,
            current_pc: 0,
            regs: [0; 32],
            out_regs: [0; 32],
            load: (0, 0),
            hi: 0,
            lo: 0,
            hi_lo_ready_at: 0,
            branch_taken: false,
            in_delay_slot: false,
            cop0: Cop0::new(),
            gte: Gte::new(),
            cycles: 0,
            stall: 0,
        };
        cpu.reset();
        cpu
    }

    /// Volta ao estado de reset: `PC` no início da BIOS, `SR.BEV` ligado.
    pub fn reset(&mut self) {
        self.pc = RESET_VECTOR;
        self.next_pc = RESET_VECTOR.wrapping_add(4);
        self.current_pc = RESET_VECTOR;
        // Os registradores gerais são indefinidos no hardware; o valor
        // "sujo" 0xDEADBEEF ajuda a flagrar leitura de registrador não
        // inicializado durante o desenvolvimento.
        self.regs = [0xDEAD_BEEF; 32];
        self.regs[0] = 0;
        self.out_regs = self.regs;
        self.load = (0, 0);
        self.hi = 0xDEAD_BEEF;
        self.lo = 0xDEAD_BEEF;
        self.hi_lo_ready_at = 0;
        self.branch_taken = false;
        self.in_delay_slot = false;
        self.cop0 = Cop0::new();
        self.cop0.sr = crate::cpu::cop0::SR_BOOT_EXCEPTION_VECTORS;
        self.gte = Gte::new();
        self.cycles = 0;
        self.stall = 0;
    }

    #[inline(always)]
    pub fn reg(&self, index: usize) -> u32 {
        self.regs[index]
    }

    #[inline(always)]
    fn set_reg(&mut self, index: usize, value: u32) {
        self.out_regs[index] = value;
        // `$zero` é constante 0. Escrever nele é permitido e simplesmente
        // não tem efeito.
        self.out_regs[0] = 0;
    }

    /// Força um registrador nos dois bancos (usado por save states e sideload).
    pub fn set_reg_direct(&mut self, index: usize, value: u32) {
        self.regs[index] = value;
        self.out_regs[index] = value;
        self.regs[0] = 0;
        self.out_regs[0] = 0;
    }

    #[inline(always)]
    pub const fn pc(&self) -> u32 {
        self.pc
    }

    #[inline(always)]
    pub const fn hi(&self) -> u32 {
        self.hi
    }

    #[inline(always)]
    pub const fn lo(&self) -> u32 {
        self.lo
    }

    #[inline(always)]
    pub const fn cycles(&self) -> u64 {
        self.cycles
    }

    /// Salta para `pc`, descartando o pipeline (usado ao dar sideload de `.exe`).
    pub fn set_pc(&mut self, pc: u32) {
        self.pc = pc;
        self.next_pc = pc.wrapping_add(4);
        self.current_pc = pc;
        self.branch_taken = false;
        self.in_delay_slot = false;
        self.load = (0, 0);
    }

    /// Executa uma instrução. Devolve quantos ciclos ela consumiu.
    pub fn step(&mut self, bus: &mut Bus) -> u32 {
        self.cop0.set_hardware_irq(bus.irq.is_pending());

        self.current_pc = self.pc;

        // Uma interrupção é reconhecida antes da instrução, então a instrução
        // interrompida é re-executada depois do `RFE`.
        if self.cop0.irq_pending() {
            self.in_delay_slot = self.branch_taken;
            self.branch_taken = false;
            self.raise(Exception::Interrupt);
            self.cycles += 1;
            return 1;
        }

        if self.current_pc & 3 != 0 {
            self.cop0.bad_vaddr = self.current_pc;
            self.in_delay_slot = self.branch_taken;
            self.branch_taken = false;
            self.raise(Exception::LoadAddressError);
            self.cycles += 1;
            return 1;
        }

        let instruction = Instruction(bus.load32(self.current_pc));

        self.pc = self.next_pc;
        self.next_pc = self.pc.wrapping_add(4);

        // O load da instrução anterior só agora fica visível.
        let (reg, value) = self.load;
        self.load = (0, 0);
        self.set_reg(reg, value);

        self.in_delay_slot = self.branch_taken;
        self.branch_taken = false;

        self.stall = 0;
        self.execute(instruction, bus);

        self.regs = self.out_regs;

        let spent = 1 + self.stall;
        self.cycles += spent as u64;
        spent
    }

    // ---------------------------------------------------------------- exceções

    fn raise(&mut self, exception: Exception) {
        self.cop0.push_exception_mode();
        self.cop0.set_exception_cause(exception, self.in_delay_slot);
        self.cop0.epc = if self.in_delay_slot {
            self.current_pc.wrapping_sub(4)
        } else {
            self.current_pc
        };
        self.pc = self.cop0.exception_handler();
        self.next_pc = self.pc.wrapping_add(4);
        self.branch_taken = false;
        // Um load pendente sobrevive à exceção e completa na primeira
        // instrução do handler, exatamente como no hardware.
    }

    // ---------------------------------------------------------------- branches

    #[inline]
    fn branch(&mut self, offset_se: u32) {
        // `self.pc` já aponta para o delay slot, então o alvo é
        // `delay_slot + offset*4`, ou seja `branch + 4 + offset*4`.
        self.next_pc = self.pc.wrapping_add(offset_se << 2);
        self.branch_taken = true;
    }

    #[inline]
    fn jump_to(&mut self, target: u32) {
        self.next_pc = target;
        self.branch_taken = true;
    }

    // ---------------------------------------------------------------- execução

    fn execute(&mut self, instruction: Instruction, bus: &mut Bus) {
        match instruction.opcode() {
            0x00 => self.execute_special(instruction),
            0x01 => self.execute_regimm(instruction),
            0x02 => {
                let target = (self.pc & 0xF000_0000) | (instruction.target() << 2);
                self.jump_to(target);
            }
            0x03 => {
                // JAL: $ra recebe o endereço depois do delay slot.
                let return_address = self.next_pc;
                let target = (self.pc & 0xF000_0000) | (instruction.target() << 2);
                self.set_reg(31, return_address);
                self.jump_to(target);
            }
            0x04 => {
                if self.reg(instruction.rs()) == self.reg(instruction.rt()) {
                    self.branch(instruction.imm_se());
                }
            }
            0x05 => {
                if self.reg(instruction.rs()) != self.reg(instruction.rt()) {
                    self.branch(instruction.imm_se());
                }
            }
            0x06 => {
                if (self.reg(instruction.rs()) as i32) <= 0 {
                    self.branch(instruction.imm_se());
                }
            }
            0x07 => {
                if (self.reg(instruction.rs()) as i32) > 0 {
                    self.branch(instruction.imm_se());
                }
            }
            0x08 => {
                // ADDI: soma com sinal, faz trap em overflow.
                let lhs = self.reg(instruction.rs()) as i32;
                let rhs = instruction.imm_se() as i32;
                match lhs.checked_add(rhs) {
                    Some(result) => self.set_reg(instruction.rt(), result as u32),
                    None => self.raise(Exception::Overflow),
                }
            }
            0x09 => {
                let value = self
                    .reg(instruction.rs())
                    .wrapping_add(instruction.imm_se());
                self.set_reg(instruction.rt(), value);
            }
            0x0A => {
                let less = (self.reg(instruction.rs()) as i32) < (instruction.imm_se() as i32);
                self.set_reg(instruction.rt(), less as u32);
            }
            0x0B => {
                let less = self.reg(instruction.rs()) < instruction.imm_se();
                self.set_reg(instruction.rt(), less as u32);
            }
            0x0C => {
                let value = self.reg(instruction.rs()) & instruction.imm();
                self.set_reg(instruction.rt(), value);
            }
            0x0D => {
                let value = self.reg(instruction.rs()) | instruction.imm();
                self.set_reg(instruction.rt(), value);
            }
            0x0E => {
                let value = self.reg(instruction.rs()) ^ instruction.imm();
                self.set_reg(instruction.rt(), value);
            }
            0x0F => self.set_reg(instruction.rt(), instruction.imm() << 16),

            0x10 => self.execute_cop0(instruction),
            0x11 | 0x13 => {
                // O PSX não tem COP1 (FPU) nem COP3.
                self.raise(Exception::CoprocessorUnusable);
            }
            0x12 => self.execute_cop2(instruction),

            0x20 => self.op_load(instruction, bus, LoadWidth::Byte, true),
            0x21 => self.op_load(instruction, bus, LoadWidth::Half, true),
            0x22 => self.op_lwl(instruction, bus),
            0x23 => self.op_load(instruction, bus, LoadWidth::Word, false),
            0x24 => self.op_load(instruction, bus, LoadWidth::Byte, false),
            0x25 => self.op_load(instruction, bus, LoadWidth::Half, false),
            0x26 => self.op_lwr(instruction, bus),

            0x28 => self.op_store(instruction, bus, LoadWidth::Byte),
            0x29 => self.op_store(instruction, bus, LoadWidth::Half),
            0x2A => self.op_swl(instruction, bus),
            0x2B => self.op_store(instruction, bus, LoadWidth::Word),
            0x2E => self.op_swr(instruction, bus),

            0x32 => self.op_lwc2(instruction, bus),
            0x3A => self.op_swc2(instruction, bus),
            0x30 | 0x31 | 0x33 | 0x38 | 0x39 | 0x3B => {
                self.raise(Exception::CoprocessorUnusable);
            }

            _ => self.raise(Exception::ReservedInstruction),
        }
    }

    fn execute_special(&mut self, instruction: Instruction) {
        match instruction.funct() {
            0x00 => {
                let value = self.reg(instruction.rt()) << instruction.shamt();
                self.set_reg(instruction.rd(), value);
            }
            0x02 => {
                let value = self.reg(instruction.rt()) >> instruction.shamt();
                self.set_reg(instruction.rd(), value);
            }
            0x03 => {
                let value = (self.reg(instruction.rt()) as i32) >> instruction.shamt();
                self.set_reg(instruction.rd(), value as u32);
            }
            0x04 => {
                // Só os 5 bits baixos de `rs` contam.
                let shift = self.reg(instruction.rs()) & 0x1F;
                let value = self.reg(instruction.rt()) << shift;
                self.set_reg(instruction.rd(), value);
            }
            0x06 => {
                let shift = self.reg(instruction.rs()) & 0x1F;
                let value = self.reg(instruction.rt()) >> shift;
                self.set_reg(instruction.rd(), value);
            }
            0x07 => {
                let shift = self.reg(instruction.rs()) & 0x1F;
                let value = (self.reg(instruction.rt()) as i32) >> shift;
                self.set_reg(instruction.rd(), value as u32);
            }
            0x08 => {
                let target = self.reg(instruction.rs());
                self.jump_to(target);
            }
            0x09 => {
                // JALR: lê `rs` antes de escrever `rd`, porque podem ser o mesmo.
                let target = self.reg(instruction.rs());
                let return_address = self.next_pc;
                self.set_reg(instruction.rd(), return_address);
                self.jump_to(target);
            }
            0x0C => self.raise(Exception::Syscall),
            0x0D => self.raise(Exception::Breakpoint),
            0x10 => {
                self.stall_for_hi_lo();
                self.set_reg(instruction.rd(), self.hi);
            }
            0x11 => self.hi = self.reg(instruction.rs()),
            0x12 => {
                self.stall_for_hi_lo();
                self.set_reg(instruction.rd(), self.lo);
            }
            0x13 => self.lo = self.reg(instruction.rs()),
            0x18 => {
                let lhs = self.reg(instruction.rs()) as i32 as i64;
                let rhs = self.reg(instruction.rt()) as i32 as i64;
                let result = (lhs * rhs) as u64;
                self.lo = result as u32;
                self.hi = (result >> 32) as u32;
                self.schedule_hi_lo(multiply_cycles(self.reg(instruction.rs()), true));
            }
            0x19 => {
                let lhs = self.reg(instruction.rs()) as u64;
                let rhs = self.reg(instruction.rt()) as u64;
                let result = lhs * rhs;
                self.lo = result as u32;
                self.hi = (result >> 32) as u32;
                self.schedule_hi_lo(multiply_cycles(self.reg(instruction.rs()), false));
            }
            0x1A => {
                let n = self.reg(instruction.rs()) as i32;
                let d = self.reg(instruction.rt()) as i32;
                // PSX-SPX: divisão por zero e o overflow de INT_MIN/-1 não
                // fazem trap; produzem estes valores.
                let (lo, hi) = if d == 0 {
                    (if n >= 0 { 0xFFFF_FFFF } else { 1 }, n as u32)
                } else if n == i32::MIN && d == -1 {
                    (0x8000_0000, 0)
                } else {
                    ((n / d) as u32, (n % d) as u32)
                };
                self.lo = lo;
                self.hi = hi;
                self.schedule_hi_lo(DIVIDE_CYCLES);
            }
            0x1B => {
                let n = self.reg(instruction.rs());
                let d = self.reg(instruction.rt());
                // A comparação explícita com zero espelha a tabela do PSX-SPX;
                // `checked_div` esconderia qual é o valor que o hardware devolve.
                #[allow(clippy::manual_checked_ops)]
                let (lo, hi) = if d == 0 {
                    (0xFFFF_FFFF, n)
                } else {
                    (n / d, n % d)
                };
                self.lo = lo;
                self.hi = hi;
                self.schedule_hi_lo(DIVIDE_CYCLES);
            }
            0x20 => {
                let lhs = self.reg(instruction.rs()) as i32;
                let rhs = self.reg(instruction.rt()) as i32;
                match lhs.checked_add(rhs) {
                    Some(result) => self.set_reg(instruction.rd(), result as u32),
                    None => self.raise(Exception::Overflow),
                }
            }
            0x21 => {
                let value = self
                    .reg(instruction.rs())
                    .wrapping_add(self.reg(instruction.rt()));
                self.set_reg(instruction.rd(), value);
            }
            0x22 => {
                let lhs = self.reg(instruction.rs()) as i32;
                let rhs = self.reg(instruction.rt()) as i32;
                match lhs.checked_sub(rhs) {
                    Some(result) => self.set_reg(instruction.rd(), result as u32),
                    None => self.raise(Exception::Overflow),
                }
            }
            0x23 => {
                let value = self
                    .reg(instruction.rs())
                    .wrapping_sub(self.reg(instruction.rt()));
                self.set_reg(instruction.rd(), value);
            }
            0x24 => {
                let value = self.reg(instruction.rs()) & self.reg(instruction.rt());
                self.set_reg(instruction.rd(), value);
            }
            0x25 => {
                let value = self.reg(instruction.rs()) | self.reg(instruction.rt());
                self.set_reg(instruction.rd(), value);
            }
            0x26 => {
                let value = self.reg(instruction.rs()) ^ self.reg(instruction.rt());
                self.set_reg(instruction.rd(), value);
            }
            0x27 => {
                let value = !(self.reg(instruction.rs()) | self.reg(instruction.rt()));
                self.set_reg(instruction.rd(), value);
            }
            0x2A => {
                let less =
                    (self.reg(instruction.rs()) as i32) < (self.reg(instruction.rt()) as i32);
                self.set_reg(instruction.rd(), less as u32);
            }
            0x2B => {
                let less = self.reg(instruction.rs()) < self.reg(instruction.rt());
                self.set_reg(instruction.rd(), less as u32);
            }
            _ => self.raise(Exception::ReservedInstruction),
        }
    }

    /// Opcode `0x01` — a família `BcondZ`, selecionada pelo campo `rt`.
    fn execute_regimm(&mut self, instruction: Instruction) {
        let rt = instruction.rt();
        let is_bgez = rt & 1 != 0;
        // Bits 1..4 == 0b1000 ligam o "and link". Qualquer outro padrão de
        // bits altos é ignorado pelo hardware.
        let should_link = (rt & 0x1E) == 0x10;

        let value = self.reg(instruction.rs()) as i32;
        let taken = if is_bgez { value >= 0 } else { value < 0 };

        // Quirk: o link acontece mesmo quando o branch não é tomado, e mesmo
        // quando `rs` é `$ra`.
        if should_link {
            let return_address = self.next_pc;
            self.set_reg(31, return_address);
        }

        if taken {
            self.branch(instruction.imm_se());
        }
    }

    fn execute_cop0(&mut self, instruction: Instruction) {
        match instruction.cop_op() {
            // MFC0 — passa pelo load delay slot, como qualquer load.
            0x00 => {
                let value = self.cop0.read(instruction.rd());
                self.load = (instruction.rt(), value);
            }
            0x04 => {
                let value = self.reg(instruction.rt());
                self.cop0.write(instruction.rd(), value);
            }
            0x10 => {
                if instruction.funct() == 0x10 {
                    self.cop0.return_from_exception();
                } else {
                    self.raise(Exception::ReservedInstruction);
                }
            }
            _ => self.raise(Exception::ReservedInstruction),
        }
    }

    fn execute_cop2(&mut self, instruction: Instruction) {
        // COP2 precisa estar habilitado em `SR.CU2` (bit 30).
        if self.cop0.sr & (1 << 30) == 0 {
            self.raise(Exception::CoprocessorUnusable);
            return;
        }

        match instruction.cop_op() {
            // MFC2 — leitura de registrador de dados, com load delay.
            0x00 => {
                let value = self.gte.read_data(instruction.rd());
                self.load = (instruction.rt(), value);
            }
            // CFC2 — leitura de registrador de controle, com load delay.
            0x02 => {
                let value = self.gte.read_control(instruction.rd());
                self.load = (instruction.rt(), value);
            }
            0x04 => {
                let value = self.reg(instruction.rt());
                self.gte.write_data(instruction.rd(), value);
            }
            0x06 => {
                let value = self.reg(instruction.rt());
                self.gte.write_control(instruction.rd(), value);
            }
            op if op & 0x10 != 0 => {
                self.stall += self.gte.execute(instruction.cop2_command());
            }
            _ => self.raise(Exception::ReservedInstruction),
        }
    }

    // -------------------------------------------------------------- load/store

    fn effective_address(&self, instruction: Instruction) -> u32 {
        self.reg(instruction.rs())
            .wrapping_add(instruction.imm_se())
    }

    fn op_load(&mut self, instruction: Instruction, bus: &mut Bus, width: LoadWidth, signed: bool) {
        let address = self.effective_address(instruction);

        if address & width.alignment_mask() != 0 {
            self.cop0.bad_vaddr = address;
            self.raise(Exception::LoadAddressError);
            return;
        }

        let value = match (width, signed) {
            (LoadWidth::Byte, true) => bus.load8(address) as i8 as u32,
            (LoadWidth::Byte, false) => bus.load8(address) as u32,
            (LoadWidth::Half, true) => bus.load16(address) as i16 as u32,
            (LoadWidth::Half, false) => bus.load16(address) as u32,
            (LoadWidth::Word, _) => bus.load32(address),
        };

        self.load = (instruction.rt(), value);
    }

    fn op_store(&mut self, instruction: Instruction, bus: &mut Bus, width: LoadWidth) {
        // Com a cache isolada a escrita fica na cache e nunca chega à memória.
        // A BIOS usa isso na rotina de inicialização.
        if self.cop0.cache_isolated() {
            return;
        }

        let address = self.effective_address(instruction);

        if address & width.alignment_mask() != 0 {
            self.cop0.bad_vaddr = address;
            self.raise(Exception::StoreAddressError);
            return;
        }

        let value = self.reg(instruction.rt());
        match width {
            LoadWidth::Byte => bus.store8(address, value as u8),
            LoadWidth::Half => bus.store16(address, value as u16),
            LoadWidth::Word => bus.store32(address, value),
        }
    }

    fn op_lwl(&mut self, instruction: Instruction, bus: &mut Bus) {
        let address = self.effective_address(instruction);
        let aligned = bus.load32(address & !3);

        // Quirk: `LWL`/`LWR` encadeados leem o valor ainda "em voo" no load
        // delay slot, por isso usamos `out_regs` e não `regs`.
        let current = self.out_regs[instruction.rt()];

        let value = match address & 3 {
            0 => (current & 0x00FF_FFFF) | (aligned << 24),
            1 => (current & 0x0000_FFFF) | (aligned << 16),
            2 => (current & 0x0000_00FF) | (aligned << 8),
            _ => aligned,
        };

        self.load = (instruction.rt(), value);
    }

    fn op_lwr(&mut self, instruction: Instruction, bus: &mut Bus) {
        let address = self.effective_address(instruction);
        let aligned = bus.load32(address & !3);
        let current = self.out_regs[instruction.rt()];

        let value = match address & 3 {
            0 => aligned,
            1 => (current & 0xFF00_0000) | (aligned >> 8),
            2 => (current & 0xFFFF_0000) | (aligned >> 16),
            _ => (current & 0xFFFF_FF00) | (aligned >> 24),
        };

        self.load = (instruction.rt(), value);
    }

    fn op_swl(&mut self, instruction: Instruction, bus: &mut Bus) {
        if self.cop0.cache_isolated() {
            return;
        }
        let address = self.effective_address(instruction);
        let aligned_address = address & !3;
        let current = bus.load32(aligned_address);
        let value = self.reg(instruction.rt());

        let merged = match address & 3 {
            0 => (current & 0xFFFF_FF00) | (value >> 24),
            1 => (current & 0xFFFF_0000) | (value >> 16),
            2 => (current & 0xFF00_0000) | (value >> 8),
            _ => value,
        };

        bus.store32(aligned_address, merged);
    }

    fn op_swr(&mut self, instruction: Instruction, bus: &mut Bus) {
        if self.cop0.cache_isolated() {
            return;
        }
        let address = self.effective_address(instruction);
        let aligned_address = address & !3;
        let current = bus.load32(aligned_address);
        let value = self.reg(instruction.rt());

        let merged = match address & 3 {
            0 => value,
            1 => (current & 0x0000_00FF) | (value << 8),
            2 => (current & 0x0000_FFFF) | (value << 16),
            _ => (current & 0x00FF_FFFF) | (value << 24),
        };

        bus.store32(aligned_address, merged);
    }

    fn op_lwc2(&mut self, instruction: Instruction, bus: &mut Bus) {
        let address = self.effective_address(instruction);
        if address & 3 != 0 {
            self.cop0.bad_vaddr = address;
            self.raise(Exception::LoadAddressError);
            return;
        }
        let value = bus.load32(address);
        self.gte.write_data(instruction.rt(), value);
    }

    fn op_swc2(&mut self, instruction: Instruction, bus: &mut Bus) {
        if self.cop0.cache_isolated() {
            return;
        }
        let address = self.effective_address(instruction);
        if address & 3 != 0 {
            self.cop0.bad_vaddr = address;
            self.raise(Exception::StoreAddressError);
            return;
        }
        let value = self.gte.read_data(instruction.rt());
        bus.store32(address, value);
    }

    // ------------------------------------------------------------- HI/LO stall

    fn schedule_hi_lo(&mut self, cycles: u32) {
        self.hi_lo_ready_at = self.cycles + cycles as u64;
    }

    fn stall_for_hi_lo(&mut self) {
        let now = self.cycles + self.stall as u64;
        if now < self.hi_lo_ready_at {
            self.stall += (self.hi_lo_ready_at - now) as u32;
        }
    }
}

impl Default for Cpu {
    fn default() -> Self {
        Self::new()
    }
}

/// Largura de um acesso à memória.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LoadWidth {
    Byte,
    Half,
    Word,
}

impl LoadWidth {
    #[inline(always)]
    const fn alignment_mask(self) -> u32 {
        match self {
            LoadWidth::Byte => 0,
            LoadWidth::Half => 1,
            LoadWidth::Word => 3,
        }
    }
}

/// PSX-SPX — "CPU Load/Store/Multiply/Divide Timings": `DIV`/`DIVU` sempre
/// levam 36 ciclos.
const DIVIDE_CYCLES: u32 = 36;

/// Ciclos de `MULT`/`MULTU`, que dependem da magnitude do primeiro operando.
fn multiply_cycles(rs: u32, signed: bool) -> u32 {
    let magnitude = if signed && (rs as i32) < 0 { !rs } else { rs };
    match magnitude {
        0x0000_0000..=0x0000_07FF => 6,
        0x0000_0800..=0x000F_FFFF => 9,
        _ => 13,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::Bus;

    /// Monta uma máquina com o programa carregado na RAM em `0x0000_0000` e o
    /// `PC` apontando para lá — evita depender de uma BIOS real nos testes.
    fn machine(program: &[u32]) -> (Cpu, Bus) {
        let mut bus = Bus::new(crate::Bios::stub());
        for (i, word) in program.iter().enumerate() {
            bus.store32(i as u32 * 4, *word);
        }
        let mut cpu = Cpu::new();
        cpu.set_pc(0x0000_0000);
        // Zera os registradores para tornar os testes determinísticos.
        for r in 0..32 {
            cpu.set_reg_direct(r, 0);
        }
        (cpu, bus)
    }

    fn run(cpu: &mut Cpu, bus: &mut Bus, instructions: usize) {
        for _ in 0..instructions {
            cpu.step(bus);
        }
    }

    #[test]
    fn zero_register_is_hardwired() {
        // addiu $zero, $zero, 0x1234
        let (mut cpu, mut bus) = machine(&[0x2400_1234]);
        run(&mut cpu, &mut bus, 1);
        assert_eq!(cpu.reg(0), 0);
    }

    #[test]
    fn lui_ori_builds_a_constant() {
        let (mut cpu, mut bus) = machine(&[
            0x3C08_1234, // lui  $t0, 0x1234
            0x3508_5678, // ori  $t0, $t0, 0x5678
        ]);
        run(&mut cpu, &mut bus, 2);
        assert_eq!(cpu.reg(8), 0x1234_5678);
    }

    #[test]
    fn load_delay_slot_exposes_old_value_first() {
        // sw $t0 em 0x1000, depois lw $t1 e imediatamente or $t2, $t1, $zero.
        // O `or` deve enxergar o valor ANTIGO de $t1, não o carregado.
        let (mut cpu, mut bus) = machine(&[
            0x3C08_ABCD, // lui  $t0, 0xABCD
            0xAC08_1000, // sw   $t0, 0x1000($zero)
            0x8C09_1000, // lw   $t1, 0x1000($zero)
            0x0120_5025, // or   $t2, $t1, $zero   <- delay slot do load
            0x0120_5825, // or   $t3, $t1, $zero   <- já enxerga o valor novo
        ]);
        cpu.set_reg_direct(9, 0x1111_1111);
        run(&mut cpu, &mut bus, 5);

        assert_eq!(cpu.reg(10), 0x1111_1111, "delay slot vê o valor antigo");
        assert_eq!(cpu.reg(11), 0xABCD_0000, "instrução seguinte vê o novo");
        assert_eq!(cpu.reg(9), 0xABCD_0000);
    }

    #[test]
    fn branch_delay_slot_always_executes() {
        // O alvo é `endereço_do_branch + 4 + offset*4`, ou seja 0x10.
        let (mut cpu, mut bus) = machine(&[
            0x1000_0003, // beq  $zero, $zero, +3
            0x2409_0007, // addiu $t1, $zero, 7     <- delay slot, executa
            0x240A_0009, // addiu $t2, $zero, 9     <- pulado
            0x0000_0000, // nop                     <- pulado
            0x240B_000B, // addiu $t3, $zero, 11    <- destino (0x10)
        ]);
        run(&mut cpu, &mut bus, 3);
        assert_eq!(cpu.reg(9), 7, "delay slot executou");
        assert_eq!(cpu.reg(10), 0, "instrução pulada não executou");
        assert_eq!(cpu.reg(11), 11, "desviou para o alvo certo");
    }

    #[test]
    fn jal_links_past_the_delay_slot() {
        let (mut cpu, mut bus) = machine(&[
            0x0C00_0004, // jal 0x10
            0x0000_0000, // nop (delay slot)
        ]);
        run(&mut cpu, &mut bus, 2);
        // $ra = endereço da instrução DEPOIS do delay slot = 0x08.
        assert_eq!(cpu.reg(31), 0x0000_0008);
        assert_eq!(cpu.pc(), 0x0000_0010);
    }

    #[test]
    fn jalr_reads_source_before_writing_destination() {
        // jalr $t0, $t0 — destino e fonte no mesmo registrador.
        let (mut cpu, mut bus) = machine(&[
            0x0100_4009, // jalr $t0, $t0
            0x0000_0000, // nop
        ]);
        cpu.set_reg_direct(8, 0x0000_0020);
        run(&mut cpu, &mut bus, 2);
        assert_eq!(cpu.pc(), 0x0000_0020, "saltou para o valor original");
        assert_eq!(cpu.reg(8), 0x0000_0008, "e depois recebeu o link");
    }

    #[test]
    fn add_traps_on_overflow_but_addu_does_not() {
        let (mut cpu, mut bus) = machine(&[
            0x0109_5020, // add  $t2, $t0, $t1
        ]);
        cpu.set_reg_direct(8, 0x7FFF_FFFF);
        cpu.set_reg_direct(9, 1);
        run(&mut cpu, &mut bus, 1);
        assert_eq!(cpu.reg(10), 0, "destino não foi escrito");
        assert_eq!((cpu.cop0.cause >> 2) & 0x1F, Exception::Overflow as u32);

        let (mut cpu, mut bus) = machine(&[
            0x0109_5021, // addu $t2, $t0, $t1
        ]);
        cpu.set_reg_direct(8, 0x7FFF_FFFF);
        cpu.set_reg_direct(9, 1);
        run(&mut cpu, &mut bus, 1);
        assert_eq!(cpu.reg(10), 0x8000_0000, "addu envolve sem exceção");
    }

    #[test]
    fn division_by_zero_is_defined_and_does_not_trap() {
        // div $t0, $t1 com $t1 = 0 e $t0 positivo.
        let (mut cpu, mut bus) = machine(&[0x0109_001A]);
        cpu.set_reg_direct(8, 42);
        cpu.set_reg_direct(9, 0);
        run(&mut cpu, &mut bus, 1);
        assert_eq!(cpu.lo(), 0xFFFF_FFFF);
        assert_eq!(cpu.hi(), 42);

        // Numerador negativo devolve LO = 1.
        let (mut cpu, mut bus) = machine(&[0x0109_001A]);
        cpu.set_reg_direct(8, (-42i32) as u32);
        cpu.set_reg_direct(9, 0);
        run(&mut cpu, &mut bus, 1);
        assert_eq!(cpu.lo(), 1);
        assert_eq!(cpu.hi(), (-42i32) as u32);
    }

    #[test]
    fn signed_division_overflow_is_defined() {
        // div INT_MIN, -1 — não representável, mas não faz trap.
        let (mut cpu, mut bus) = machine(&[0x0109_001A]);
        cpu.set_reg_direct(8, 0x8000_0000);
        cpu.set_reg_direct(9, 0xFFFF_FFFF);
        run(&mut cpu, &mut bus, 1);
        assert_eq!(cpu.lo(), 0x8000_0000);
        assert_eq!(cpu.hi(), 0);
    }

    #[test]
    fn unaligned_word_load_raises_address_error() {
        let (mut cpu, mut bus) = machine(&[0x8C08_0001]); // lw $t0, 1($zero)
        run(&mut cpu, &mut bus, 1);
        assert_eq!(
            (cpu.cop0.cause >> 2) & 0x1F,
            Exception::LoadAddressError as u32
        );
        assert_eq!(cpu.cop0.bad_vaddr, 1);
    }

    #[test]
    fn lwl_lwr_pair_loads_unaligned_word() {
        let (mut cpu, mut bus) = machine(&[
            0x8828_1003, // lwl $t0, 0x1003($zero)
            0x9828_1000, // lwr $t0, 0x1000($zero)
            0x0000_0000, // nop (completa o load delay)
        ]);
        // Bytes em 0x1000: 11 22 33 44
        bus.store32(0x1000, 0x4433_2211);
        run(&mut cpu, &mut bus, 3);
        assert_eq!(cpu.reg(8), 0x4433_2211);
    }

    #[test]
    fn swl_swr_pair_stores_unaligned_word() {
        let (mut cpu, mut bus) = machine(&[
            0xA828_1003, // swl $t0, 0x1003($zero)
            0xB828_1000, // swr $t0, 0x1000($zero)
        ]);
        cpu.set_reg_direct(8, 0xAABB_CCDD);
        run(&mut cpu, &mut bus, 2);
        assert_eq!(bus.load32(0x1000), 0xAABB_CCDD);
    }

    #[test]
    fn isolated_cache_swallows_stores() {
        let (mut cpu, mut bus) = machine(&[
            0xAC08_1000, // sw $t0, 0x1000($zero)
        ]);
        cpu.set_reg_direct(8, 0xFFFF_FFFF);
        cpu.cop0.sr |= crate::cpu::cop0::SR_ISOLATE_CACHE;
        run(&mut cpu, &mut bus, 1);
        assert_eq!(bus.load32(0x1000), 0, "store não chegou à RAM");
    }

    #[test]
    fn syscall_jumps_to_handler_and_saves_epc() {
        let (mut cpu, mut bus) = machine(&[
            0x0000_000C, // syscall
        ]);
        // BEV desligado → handler em 0x80000080.
        cpu.cop0.sr = 0;
        run(&mut cpu, &mut bus, 1);
        assert_eq!(cpu.pc(), 0x8000_0080);
        assert_eq!(cpu.cop0.epc, 0x0000_0000);
        assert_eq!((cpu.cop0.cause >> 2) & 0x1F, Exception::Syscall as u32);
        assert_eq!(cpu.cop0.cause & crate::cpu::cop0::CAUSE_BRANCH_DELAY, 0);
    }

    #[test]
    fn exception_in_delay_slot_points_epc_at_the_branch() {
        let (mut cpu, mut bus) = machine(&[
            0x1000_0002, // beq $zero, $zero, +2   (endereço 0x00)
            0x0000_000C, // syscall                (endereço 0x04, delay slot)
        ]);
        cpu.cop0.sr = 0;
        run(&mut cpu, &mut bus, 2);
        assert_eq!(cpu.cop0.epc, 0x0000_0000, "EPC aponta para o branch");
        assert_ne!(
            cpu.cop0.cause & crate::cpu::cop0::CAUSE_BRANCH_DELAY,
            0,
            "Cause.BD ligado"
        );
    }

    #[test]
    fn bgezal_links_even_when_branch_not_taken() {
        // bgezal $t0, +4 com $t0 negativo: não desvia, mas $ra é escrito.
        let (mut cpu, mut bus) = machine(&[
            0x0511_0004, // bgezal $t0, +4
            0x0000_0000, // nop
        ]);
        cpu.set_reg_direct(8, 0xFFFF_FFFF);
        run(&mut cpu, &mut bus, 2);
        assert_eq!(cpu.reg(31), 0x0000_0008, "link acontece mesmo sem desviar");
        assert_eq!(cpu.pc(), 0x0000_0008, "e o fluxo seguiu em frente");
    }

    #[test]
    fn cop1_and_cop3_are_unusable() {
        for opcode in [0x11u32, 0x13] {
            let (mut cpu, mut bus) = machine(&[opcode << 26]);
            run(&mut cpu, &mut bus, 1);
            assert_eq!(
                (cpu.cop0.cause >> 2) & 0x1F,
                Exception::CoprocessorUnusable as u32,
                "opcode {opcode:#04X}"
            );
        }
    }

    #[test]
    fn rfe_restores_previous_mode() {
        let (mut cpu, mut bus) = machine(&[
            0x4210_0010, // rfe
        ]);
        cpu.cop0.sr = 0b00_1100; // previous: IEp=1, KUp=1
        run(&mut cpu, &mut bus, 1);
        assert_eq!(cpu.cop0.sr & 0xF, 0b0011);
    }

    #[test]
    fn shift_variable_uses_only_five_bits() {
        // sllv $t2, $t1, $t0 com $t0 = 33 → shift de 1.
        let (mut cpu, mut bus) = machine(&[0x0109_5004]);
        cpu.set_reg_direct(8, 33);
        cpu.set_reg_direct(9, 1);
        run(&mut cpu, &mut bus, 1);
        assert_eq!(cpu.reg(10), 2);
    }

    #[test]
    fn multiply_timing_depends_on_operand_magnitude() {
        assert_eq!(multiply_cycles(0x0000_0000, false), 6);
        assert_eq!(multiply_cycles(0x0000_07FF, false), 6);
        assert_eq!(multiply_cycles(0x0000_0800, false), 9);
        assert_eq!(multiply_cycles(0x000F_FFFF, false), 9);
        assert_eq!(multiply_cycles(0x0010_0000, false), 13);
        // Com sinal, o que conta é a magnitude.
        assert_eq!(multiply_cycles(0xFFFF_FFFF, true), 6);
        assert_eq!(multiply_cycles(0xFFFF_FFFF, false), 13);
    }

    #[test]
    fn mflo_stalls_until_divide_completes() {
        let (mut cpu, mut bus) = machine(&[
            0x0109_001B, // divu $t0, $t1
            0x0000_5012, // mflo $t2
        ]);
        cpu.set_reg_direct(8, 100);
        cpu.set_reg_direct(9, 7);

        let div_cycles = cpu.step(&mut bus);
        assert_eq!(div_cycles, 1, "a própria divisão não trava a CPU");
        let mflo_cycles = cpu.step(&mut bus);
        assert!(
            mflo_cycles > 1,
            "MFLO deve travar esperando o divisor: {mflo_cycles}"
        );
        assert_eq!(cpu.reg(10), 14);
    }

    #[test]
    fn reset_puts_pc_on_the_bios_entry_point() {
        let cpu = Cpu::new();
        assert_eq!(cpu.pc(), RESET_VECTOR);
        assert_eq!(cpu.cop0.exception_handler(), 0xBFC0_0180, "BEV ligado");
    }
}
