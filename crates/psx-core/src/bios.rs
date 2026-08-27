//! Carregamento e identificação da BIOS.
//!
//! Referência: PSX-SPX — "BIOS Memory Map".
//!
//! **Nenhuma BIOS é embutida neste repositório.** O usuário deve fornecer o
//! dump do console que possui legalmente.

use crate::PsxError;

/// Tamanho obrigatório de uma imagem de BIOS do PSX.
pub const BIOS_SIZE: usize = 512 * 1024;

/// ROM da BIOS mapeada em `0x1FC0_0000` (KSEG1: `0xBFC0_0000`), somente leitura.
#[derive(Clone)]
pub struct Bios {
    data: Box<[u8]>,
}

impl Bios {
    /// Valida e carrega uma imagem de BIOS de 512 KB.
    pub fn new(data: Vec<u8>) -> Result<Self, PsxError> {
        if data.len() != BIOS_SIZE {
            return Err(PsxError::InvalidBiosSize(data.len()));
        }
        Ok(Self {
            data: data.into_boxed_slice(),
        })
    }

    /// BIOS de placeholder preenchida com zeros, usada apenas em testes que
    /// exercitam o bus sem precisar de uma ROM real.
    pub fn stub() -> Self {
        Self {
            data: vec![0; BIOS_SIZE].into_boxed_slice(),
        }
    }

    #[inline(always)]
    pub fn read8(&self, offset: u32) -> u8 {
        self.data[offset as usize]
    }

    #[inline(always)]
    pub fn read16(&self, offset: u32) -> u16 {
        let i = offset as usize;
        u16::from_le_bytes([self.data[i], self.data[i + 1]])
    }

    #[inline(always)]
    pub fn read32(&self, offset: u32) -> u32 {
        let i = offset as usize;
        u32::from_le_bytes([
            self.data[i],
            self.data[i + 1],
            self.data[i + 2],
            self.data[i + 3],
        ])
    }

    /// Procura a string de data de build que toda BIOS oficial carrega em
    /// ASCII (formato `YYYY-MM-DD`), útil para identificar a revisão na UI.
    pub fn build_date(&self) -> Option<String> {
        // A data fica na primeira página da ROM em todas as revisões conhecidas.
        let window = &self.data[..0x1_0000.min(self.data.len())];
        window.windows(10).find_map(|w| {
            let ok = w[0..4].iter().all(u8::is_ascii_digit)
                && w[4] == b'-'
                && w[5..7].iter().all(u8::is_ascii_digit)
                && w[7] == b'-'
                && w[8..10].iter().all(u8::is_ascii_digit);
            ok.then(|| String::from_utf8_lossy(w).into_owned())
        })
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.data
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_wrong_size() {
        assert_eq!(
            Bios::new(vec![0; 1024]).err(),
            Some(PsxError::InvalidBiosSize(1024))
        );
        assert_eq!(
            Bios::new(Vec::new()).err(),
            Some(PsxError::InvalidBiosSize(0))
        );
    }

    #[test]
    fn accepts_exact_size() {
        assert!(Bios::new(vec![0; BIOS_SIZE]).is_ok());
    }

    #[test]
    fn finds_build_date_string() {
        // Fixture sintética: nunca usamos bytes de uma BIOS real em teste.
        let mut data = vec![0u8; BIOS_SIZE];
        data[0x100..0x10A].copy_from_slice(b"1995-05-25");
        let bios = Bios::new(data).unwrap();
        assert_eq!(bios.build_date().as_deref(), Some("1995-05-25"));
    }

    #[test]
    fn build_date_absent_is_none() {
        assert_eq!(Bios::stub().build_date(), None);
    }
}
