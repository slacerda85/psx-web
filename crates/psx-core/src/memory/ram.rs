//! RAM principal e scratchpad.
//!
//! Referência: PSX-SPX — "Memory Map".

/// 2 MB de RAM principal.
pub const RAM_SIZE: usize = 2 * 1024 * 1024;
/// 1 KB de scratchpad (a D-Cache do R3000A usada como fast RAM).
pub const SCRATCHPAD_SIZE: usize = 1024;

/// RAM principal com espelhamento de 2 MB em 8 MB.
#[derive(Clone)]
pub struct Ram {
    data: Box<[u8]>,
}

impl Ram {
    /// Cria a RAM.
    ///
    /// O hardware liga com conteúdo indefinido; usamos zeros porque a BIOS
    /// inicializa tudo que consome antes de ler.
    pub fn new() -> Self {
        Self {
            data: vec![0; RAM_SIZE].into_boxed_slice(),
        }
    }

    /// Offset físico → índice dentro dos 2 MB reais (espelha 4×).
    #[inline(always)]
    const fn wrap(offset: u32) -> usize {
        (offset & (RAM_SIZE as u32 - 1)) as usize
    }

    #[inline(always)]
    pub fn read8(&self, offset: u32) -> u8 {
        self.data[Self::wrap(offset)]
    }

    #[inline(always)]
    pub fn read16(&self, offset: u32) -> u16 {
        let i = Self::wrap(offset);
        u16::from_le_bytes([self.data[i], self.data[(i + 1) & (RAM_SIZE - 1)]])
    }

    #[inline(always)]
    pub fn read32(&self, offset: u32) -> u32 {
        let i = Self::wrap(offset);
        u32::from_le_bytes([
            self.data[i],
            self.data[(i + 1) & (RAM_SIZE - 1)],
            self.data[(i + 2) & (RAM_SIZE - 1)],
            self.data[(i + 3) & (RAM_SIZE - 1)],
        ])
    }

    #[inline(always)]
    pub fn write8(&mut self, offset: u32, value: u8) {
        self.data[Self::wrap(offset)] = value;
    }

    #[inline(always)]
    pub fn write16(&mut self, offset: u32, value: u16) {
        let i = Self::wrap(offset);
        let b = value.to_le_bytes();
        self.data[i] = b[0];
        self.data[(i + 1) & (RAM_SIZE - 1)] = b[1];
    }

    #[inline(always)]
    pub fn write32(&mut self, offset: u32, value: u32) {
        let i = Self::wrap(offset);
        let b = value.to_le_bytes();
        self.data[i] = b[0];
        self.data[(i + 1) & (RAM_SIZE - 1)] = b[1];
        self.data[(i + 2) & (RAM_SIZE - 1)] = b[2];
        self.data[(i + 3) & (RAM_SIZE - 1)] = b[3];
    }

    /// Acesso direto para carga de executáveis e save states.
    pub fn as_slice(&self) -> &[u8] {
        &self.data
    }

    /// Acesso direto mutável para carga de executáveis e save states.
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        &mut self.data
    }
}

impl Default for Ram {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn little_endian_round_trip() {
        let mut ram = Ram::new();
        ram.write32(0x100, 0xDEAD_BEEF);
        assert_eq!(ram.read32(0x100), 0xDEAD_BEEF);
        // O PSX é little-endian: o byte menos significativo vem primeiro.
        assert_eq!(ram.read8(0x100), 0xEF);
        assert_eq!(ram.read8(0x101), 0xBE);
        assert_eq!(ram.read8(0x102), 0xAD);
        assert_eq!(ram.read8(0x103), 0xDE);
        assert_eq!(ram.read16(0x100), 0xBEEF);
        assert_eq!(ram.read16(0x102), 0xDEAD);
    }

    #[test]
    fn two_megabytes_mirror_four_times() {
        let mut ram = Ram::new();
        ram.write32(0, 0x1234_5678);
        for mirror in 1..4u32 {
            let offset = mirror * RAM_SIZE as u32;
            assert_eq!(ram.read32(offset), 0x1234_5678, "espelho {mirror}");
        }
        // Escrever no espelho altera o original.
        ram.write32(3 * RAM_SIZE as u32, 0xAABB_CCDD);
        assert_eq!(ram.read32(0), 0xAABB_CCDD);
    }
}
