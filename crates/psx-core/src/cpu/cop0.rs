//! Coprocessador 0 — controle do sistema e exceções.
//!
//! Referência: PSX-SPX — "COP0 - Exception Handling".

/// Códigos de exceção gravados em `Cause.ExcCode` (bits 2..6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum Exception {
    /// Interrupção externa ou de software.
    Interrupt = 0x00,
    /// Endereço inválido ou desalinhado numa leitura.
    LoadAddressError = 0x04,
    /// Endereço inválido ou desalinhado numa escrita.
    StoreAddressError = 0x05,
    /// Erro de barramento buscando instrução.
    BusErrorInstruction = 0x06,
    /// Erro de barramento em load/store.
    BusErrorData = 0x07,
    /// `SYSCALL`.
    Syscall = 0x08,
    /// `BREAK`.
    Breakpoint = 0x09,
    /// Opcode não reconhecido.
    ReservedInstruction = 0x0A,
    /// Acesso a coprocessador desabilitado ou inexistente.
    CoprocessorUnusable = 0x0B,
    /// Overflow em `ADD`/`ADDI`/`SUB`.
    Overflow = 0x0C,
}

/// Bit de `SR` que isola a cache. Enquanto ligado, stores vão para a cache e
/// não alcançam a memória — a BIOS usa isso para inicializar a scratchpad.
pub const SR_ISOLATE_CACHE: u32 = 1 << 16;
/// Bit de `SR` que move os vetores de exceção para a ROM (`0xBFC0_0180`).
pub const SR_BOOT_EXCEPTION_VECTORS: u32 = 1 << 22;
/// Bit de `Cause` que marca "exceção ocorreu num branch delay slot".
pub const CAUSE_BRANCH_DELAY: u32 = 1 << 31;
/// Bit de `Cause` correspondente à linha de IRQ do hardware (IP2).
pub const CAUSE_HARDWARE_IRQ: u32 = 1 << 10;

/// Registradores do COP0.
#[derive(Debug, Clone, Copy, Default)]
pub struct Cop0 {
    /// `cop0r12` — Status Register.
    pub sr: u32,
    /// `cop0r13` — Cause.
    pub cause: u32,
    /// `cop0r14` — Exception Program Counter.
    pub epc: u32,
    /// `cop0r8` — endereço que causou o address error.
    pub bad_vaddr: u32,
    /// `cop0r3` — breakpoint on execute.
    pub bpc: u32,
    /// `cop0r5` — breakpoint on data access.
    pub bda: u32,
    /// `cop0r6` — jump destination (somente leitura no hardware).
    pub jump_dest: u32,
    /// `cop0r7` — breakpoint control.
    pub dcic: u32,
    /// `cop0r9` — máscara do breakpoint de dados.
    pub bdam: u32,
    /// `cop0r11` — máscara do breakpoint de execução.
    pub bpcm: u32,
}

impl Cop0 {
    pub const fn new() -> Self {
        Self {
            sr: 0,
            cause: 0,
            epc: 0,
            bad_vaddr: 0,
            bpc: 0,
            bda: 0,
            jump_dest: 0,
            dcic: 0,
            bdam: 0,
            bpcm: 0,
        }
    }

    /// Interrupções globalmente habilitadas (`SR.IEc`).
    #[inline]
    pub const fn interrupts_enabled(&self) -> bool {
        self.sr & 1 != 0
    }

    /// Cache isolada (`SR.IsC`): stores não chegam à memória.
    #[inline]
    pub const fn cache_isolated(&self) -> bool {
        self.sr & SR_ISOLATE_CACHE != 0
    }

    /// Endereço base do handler de exceção, conforme `SR.BEV`.
    #[inline]
    pub const fn exception_handler(&self) -> u32 {
        if self.sr & SR_BOOT_EXCEPTION_VECTORS != 0 {
            0xBFC0_0180
        } else {
            0x8000_0080
        }
    }

    /// Reflete a linha de IRQ do controlador de interrupções em `Cause.IP2`.
    #[inline]
    pub fn set_hardware_irq(&mut self, active: bool) {
        if active {
            self.cause |= CAUSE_HARDWARE_IRQ;
        } else {
            self.cause &= !CAUSE_HARDWARE_IRQ;
        }
    }

    /// `true` quando há interrupção pendente e desmascarada, com `IEc` ligado.
    #[inline]
    pub const fn irq_pending(&self) -> bool {
        let pending = (self.cause >> 8) & 0xFF;
        let enabled = (self.sr >> 8) & 0xFF;
        self.interrupts_enabled() && (pending & enabled) != 0
    }

    /// Empilha o modo atual (IE/KU) ao entrar numa exceção.
    #[inline]
    pub fn push_exception_mode(&mut self) {
        let mode = self.sr & 0x3F;
        self.sr &= !0x3F;
        self.sr |= (mode << 2) & 0x3F;
    }

    /// `RFE` — desempilha o modo salvo por [`Self::push_exception_mode`].
    ///
    /// Só os bits 0..3 voltam; os bits 4..5 (modo "old") permanecem, exatamente
    /// como no hardware.
    #[inline]
    pub fn return_from_exception(&mut self) {
        let mode = self.sr & 0x3F;
        self.sr &= !0xF;
        self.sr |= mode >> 2;
    }

    /// Grava `ExcCode` e o bit de branch delay em `Cause`.
    #[inline]
    pub fn set_exception_cause(&mut self, exception: Exception, in_delay_slot: bool) {
        self.cause &= !0x7C;
        self.cause |= (exception as u32) << 2;
        if in_delay_slot {
            self.cause |= CAUSE_BRANCH_DELAY;
        } else {
            self.cause &= !CAUSE_BRANCH_DELAY;
        }
    }

    /// `MFC0` — leitura de um registrador do COP0.
    pub fn read(&self, index: usize) -> u32 {
        match index {
            3 => self.bpc,
            5 => self.bda,
            6 => self.jump_dest,
            7 => self.dcic,
            8 => self.bad_vaddr,
            9 => self.bdam,
            11 => self.bpcm,
            12 => self.sr,
            13 => self.cause,
            14 => self.epc,
            // cop0r15 — PRid. 0x00000002 identifica o R3000A do PSX.
            15 => 0x0000_0002,
            // Registradores não implementados leem zero no hardware.
            _ => 0,
        }
    }

    /// `MTC0` — escrita num registrador do COP0.
    ///
    /// Devolve `true` se a escrita pode ter tornado uma interrupção pendente
    /// (ou seja, mexeu em `SR` ou nos bits de software de `Cause`).
    pub fn write(&mut self, index: usize, value: u32) -> bool {
        match index {
            3 => self.bpc = value,
            5 => self.bda = value,
            6 => self.jump_dest = value,
            7 => self.dcic = value,
            9 => self.bdam = value,
            11 => self.bpcm = value,
            12 => {
                self.sr = value;
                return true;
            }
            13 => {
                // Só os dois bits de interrupção por software são graváveis.
                self.cause &= !0x0300;
                self.cause |= value & 0x0300;
                return true;
            }
            14 => self.epc = value,
            _ => {}
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exception_vector_follows_bev() {
        let mut cop0 = Cop0::new();
        cop0.sr = SR_BOOT_EXCEPTION_VECTORS;
        assert_eq!(cop0.exception_handler(), 0xBFC0_0180);
        cop0.sr = 0;
        assert_eq!(cop0.exception_handler(), 0x8000_0080);
    }

    #[test]
    fn mode_stack_pushes_and_pops() {
        let mut cop0 = Cop0::new();
        // IEc=1, KUc=0 (kernel, interrupções ligadas).
        cop0.sr = 0b00_0001;
        cop0.push_exception_mode();
        // current vira previous; current fica zerado (kernel, IRQ off).
        assert_eq!(cop0.sr & 0x3F, 0b00_0100);
        assert!(!cop0.interrupts_enabled());

        cop0.return_from_exception();
        assert_eq!(cop0.sr & 0xF, 0b0001);
        assert!(cop0.interrupts_enabled());
    }

    #[test]
    fn irq_pending_requires_mask_and_global_enable() {
        let mut cop0 = Cop0::new();
        cop0.set_hardware_irq(true);
        assert!(!cop0.irq_pending(), "sem IM nem IEc");

        // IM2 ligado, mas IEc desligado.
        cop0.sr = 1 << 10;
        assert!(!cop0.irq_pending());

        // IEc ligado.
        cop0.sr |= 1;
        assert!(cop0.irq_pending());

        cop0.set_hardware_irq(false);
        assert!(!cop0.irq_pending());
    }

    #[test]
    fn cause_exccode_and_branch_delay() {
        let mut cop0 = Cop0::new();
        cop0.set_exception_cause(Exception::Syscall, false);
        assert_eq!((cop0.cause >> 2) & 0x1F, 0x08);
        assert_eq!(cop0.cause & CAUSE_BRANCH_DELAY, 0);

        cop0.set_exception_cause(Exception::Overflow, true);
        assert_eq!((cop0.cause >> 2) & 0x1F, 0x0C);
        assert_ne!(cop0.cause & CAUSE_BRANCH_DELAY, 0);
    }

    #[test]
    fn cause_is_mostly_read_only() {
        let mut cop0 = Cop0::new();
        cop0.set_exception_cause(Exception::Syscall, false);
        // Tentar sobrescrever ExcCode via MTC0 não tem efeito.
        cop0.write(13, 0xFFFF_FFFF);
        assert_eq!((cop0.cause >> 2) & 0x1F, 0x08);
        // Mas os bits de IRQ por software (8 e 9) são graváveis.
        assert_eq!(cop0.cause & 0x0300, 0x0300);
    }

    #[test]
    fn prid_identifies_r3000a() {
        assert_eq!(Cop0::new().read(15), 0x0000_0002);
    }
}
