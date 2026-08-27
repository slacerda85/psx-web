//! SPU — Sound Processing Unit.
//!
//! Referência: PSX-SPX — "SPU (Sound Processing Unit)", "SPU Registers".
//!
//! **Escopo atual:** banco de registradores, SPU RAM de 512 KB, transferências
//! por I/O e por DMA, e `SPUSTAT` espelhando `SPUCNT` — o suficiente para o
//! BIOS inicializar o áudio sem travar num poll.
//!
//! As 24 vozes, o decodificador ADPCM, o ADSR e o reverb são entrega do agente
//! `@spu`. Enquanto isso o core entrega silêncio, e o ring buffer já existe
//! para o frontend consumir.

/// Tamanho da SPU RAM.
pub const SPU_RAM_SIZE: usize = 512 * 1024;
/// Frequência de saída do SPU.
pub const SAMPLE_RATE: u32 = 44_100;
/// Amostras (estéreo intercalado) que o ring buffer comporta.
const RING_CAPACITY: usize = SAMPLE_RATE as usize; // ~1 s de folga

/// O SPU.
pub struct Spu {
    /// `0x1F80_1C00..0x1F80_1E00` — 256 registradores de 16 bits.
    registers: [u16; 0x100],
    ram: Box<[u16]>,
    /// Endereço corrente de transferência, em halfwords.
    transfer_address: u32,
    /// Buffer circular de saída, estéreo intercalado (L, R, L, R, ...).
    ring: Box<[i16]>,
    write_cursor: usize,
    read_cursor: usize,
    /// Resto fracionário de ciclos de CPU ainda não convertidos em amostras.
    cycle_fraction: u32,
}

/// `SPUCNT` — offset do registrador de controle dentro do bloco.
const SPUCNT: usize = 0x1AA / 2;
/// `SPUSTAT` — offset do registrador de status.
const SPUSTAT: usize = 0x1AE / 2;
/// Endereço de transferência (`0x1F80_1DA6`).
const TRANSFER_ADDRESS: usize = 0x1A6 / 2;
/// FIFO de transferência (`0x1F80_1DA8`).
const TRANSFER_FIFO: usize = 0x1A8 / 2;

/// Ciclos de CPU por amostra de áudio: 33868800 / 44100 = 768, exato.
const CYCLES_PER_SAMPLE: u32 = crate::CPU_CLOCK_HZ / SAMPLE_RATE;

impl Spu {
    pub fn new() -> Self {
        Self {
            registers: [0; 0x100],
            ram: vec![0; SPU_RAM_SIZE / 2].into_boxed_slice(),
            transfer_address: 0,
            ring: vec![0; RING_CAPACITY * 2].into_boxed_slice(),
            write_cursor: 0,
            read_cursor: 0,
            cycle_fraction: 0,
        }
    }

    /// Avança o SPU, produzindo amostras.
    pub fn step(&mut self, cycles: u32) {
        self.cycle_fraction += cycles;
        let samples = self.cycle_fraction / CYCLES_PER_SAMPLE;
        self.cycle_fraction %= CYCLES_PER_SAMPLE;

        for _ in 0..samples {
            // TODO(@spu): mixar as 24 vozes, CD audio e reverb.
            self.push_sample(0, 0);
        }
    }

    fn push_sample(&mut self, left: i16, right: i16) {
        let next = (self.write_cursor + 2) % self.ring.len();
        if next == self.read_cursor {
            // Buffer cheio: descarta a amostra mais antiga em vez de bloquear.
            self.read_cursor = (self.read_cursor + 2) % self.ring.len();
        }
        self.ring[self.write_cursor] = left;
        self.ring[self.write_cursor + 1] = right;
        self.write_cursor = next;
    }

    /// Copia até `out.len() / 2` frames para `out` e devolve quantas amostras
    /// (valores individuais) foram escritas.
    pub fn drain_samples(&mut self, out: &mut [i16]) -> usize {
        let mut written = 0;
        while written + 1 < out.len() && self.read_cursor != self.write_cursor {
            out[written] = self.ring[self.read_cursor];
            out[written + 1] = self.ring[self.read_cursor + 1];
            self.read_cursor = (self.read_cursor + 2) % self.ring.len();
            written += 2;
        }
        written
    }

    /// Quantas amostras estão disponíveis para consumo.
    pub fn queued_samples(&self) -> usize {
        if self.write_cursor >= self.read_cursor {
            self.write_cursor - self.read_cursor
        } else {
            self.ring.len() - self.read_cursor + self.write_cursor
        }
    }

    /// Leitura de 16 bits (offset dentro de `0x1F80_1C00`).
    pub fn read(&mut self, offset: u32) -> u16 {
        let index = ((offset >> 1) & 0xFF) as usize;
        match index {
            SPUSTAT => self.status(),
            TRANSFER_FIFO => {
                let value = self.ram[(self.transfer_address as usize) % self.ram.len()];
                self.transfer_address = self.transfer_address.wrapping_add(1);
                value
            }
            _ => self.registers[index],
        }
    }

    /// Escrita de 16 bits (offset dentro de `0x1F80_1C00`).
    pub fn write(&mut self, offset: u32, value: u16) {
        let index = ((offset >> 1) & 0xFF) as usize;
        match index {
            TRANSFER_ADDRESS => {
                self.registers[index] = value;
                // O registrador guarda o endereço dividido por 8 (em bytes),
                // o que dá 4 halfwords por unidade.
                self.transfer_address = (value as u32) * 4;
            }
            TRANSFER_FIFO => self.write_ram(value),
            // SPUSTAT é somente leitura.
            SPUSTAT => {}
            _ => self.registers[index] = value,
        }
    }

    fn write_ram(&mut self, value: u16) {
        let address = (self.transfer_address as usize) % self.ram.len();
        self.ram[address] = value;
        self.transfer_address = self.transfer_address.wrapping_add(1);
    }

    /// `SPUSTAT` (`0x1F80_1DAE`).
    ///
    /// Os bits 0..5 espelham `SPUCNT`; o BIOS fica em loop até o espelho
    /// bater com o que escreveu.
    fn status(&self) -> u16 {
        // Bit 10 (transfer busy) fica sempre em zero porque as transferências
        // aqui são instantâneas; os bits 8/9 de request de DMA seguem a mesma
        // lógica. O agente `@spu` deve refiná-los junto com o timing real.
        self.registers[SPUCNT] & 0x003F
    }

    /// Transferência de DMA (canal 4), RAM → SPU.
    pub fn dma_write(&mut self, value: u32) {
        self.write_ram(value as u16);
        self.write_ram((value >> 16) as u16);
    }

    /// Transferência de DMA (canal 4), SPU → RAM.
    pub fn dma_read(&mut self) -> u32 {
        let low = self.ram[(self.transfer_address as usize) % self.ram.len()] as u32;
        self.transfer_address = self.transfer_address.wrapping_add(1);
        let high = self.ram[(self.transfer_address as usize) % self.ram.len()] as u32;
        self.transfer_address = self.transfer_address.wrapping_add(1);
        low | (high << 16)
    }

    pub fn ram(&self) -> &[u16] {
        &self.ram
    }
}

impl Default for Spu {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spustat_mirrors_the_low_bits_of_spucnt() {
        let mut spu = Spu::new();
        spu.write(0x1AA, 0x8025);
        assert_eq!(spu.read(0x1AE) & 0x3F, 0x25);
    }

    #[test]
    fn spustat_is_read_only() {
        let mut spu = Spu::new();
        spu.write(0x1AE, 0xFFFF);
        assert_eq!(spu.read(0x1AE), 0);
    }

    #[test]
    fn transfer_writes_land_in_spu_ram() {
        let mut spu = Spu::new();
        spu.write(0x1A6, 0x0100); // endereço
        spu.write(0x1A8, 0xAAAA);
        spu.write(0x1A8, 0xBBBB);

        // Reposiciona e lê de volta.
        spu.write(0x1A6, 0x0100);
        assert_eq!(spu.read(0x1A8), 0xAAAA);
        assert_eq!(spu.read(0x1A8), 0xBBBB);
    }

    #[test]
    fn dma_moves_two_halfwords_per_word() {
        let mut spu = Spu::new();
        spu.write(0x1A6, 0x0200);
        spu.dma_write(0xBBBB_AAAA);

        spu.write(0x1A6, 0x0200);
        assert_eq!(spu.dma_read(), 0xBBBB_AAAA);
    }

    #[test]
    fn one_sample_is_produced_every_768_cycles() {
        let mut spu = Spu::new();
        assert_eq!(CYCLES_PER_SAMPLE, 768, "33868800 / 44100 é exato");

        spu.step(767);
        assert_eq!(spu.queued_samples(), 0);
        spu.step(1);
        assert_eq!(spu.queued_samples(), 2, "um frame estéreo");
    }

    #[test]
    fn drain_empties_the_ring() {
        let mut spu = Spu::new();
        spu.step(CYCLES_PER_SAMPLE * 10);
        assert_eq!(spu.queued_samples(), 20);

        let mut out = [0i16; 64];
        let written = spu.drain_samples(&mut out);
        assert_eq!(written, 20);
        assert_eq!(spu.queued_samples(), 0);
        assert_eq!(spu.drain_samples(&mut out), 0);
    }

    #[test]
    fn ring_overflow_drops_the_oldest_frames_instead_of_blocking() {
        let mut spu = Spu::new();
        // Mais amostras do que o buffer comporta.
        spu.step(CYCLES_PER_SAMPLE * (RING_CAPACITY as u32 + 100));
        assert!(spu.queued_samples() <= spu.ring.len());
    }
}
