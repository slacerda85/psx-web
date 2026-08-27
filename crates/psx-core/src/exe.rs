//! Carregamento de executáveis `PS-X EXE`.
//!
//! Referência: PSX-SPX — "PSX EXE Header".
//!
//! Um `.exe`/`.psexe` é um cabeçalho de 2048 bytes seguido do código. É o
//! formato usado por homebrew e por praticamente todas as suítes de teste da
//! comunidade, então serve como caminho de execução sem precisar de disco.

use crate::PsxError;

/// Tamanho fixo do cabeçalho.
pub const HEADER_SIZE: usize = 0x800;

/// Cabeçalho decodificado de um `PS-X EXE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExeHeader {
    /// Valor inicial de `PC`.
    pub initial_pc: u32,
    /// Valor inicial de `$gp` (`r28`).
    pub initial_gp: u32,
    /// Endereço de RAM onde o corpo é carregado.
    pub destination: u32,
    /// Tamanho do corpo, múltiplo de 2048.
    pub size: u32,
    /// Região a zerar antes de saltar (`0` = não zerar).
    pub memfill_start: u32,
    pub memfill_size: u32,
    /// Base e offset que compõem o `$sp`/`$fp` iniciais.
    pub stack_base: u32,
    pub stack_offset: u32,
}

impl ExeHeader {
    /// Valor inicial de `$sp` e `$fp`. Quando a base é zero, o BIOS mantém a
    /// pilha que já estava configurada.
    pub const fn initial_sp(&self) -> Option<u32> {
        if self.stack_base == 0 {
            None
        } else {
            Some(self.stack_base.wrapping_add(self.stack_offset))
        }
    }
}

/// Um executável pronto para ser carregado.
#[derive(Debug, Clone)]
pub struct Executable {
    pub header: ExeHeader,
    /// Corpo do executável, sem o cabeçalho.
    pub body: Vec<u8>,
}

impl Executable {
    /// Valida e decodifica um `PS-X EXE`.
    pub fn parse(data: &[u8]) -> Result<Self, PsxError> {
        if data.len() < HEADER_SIZE {
            return Err(PsxError::TruncatedExe(data.len()));
        }
        if &data[0..8] != b"PS-X EXE" {
            return Err(PsxError::InvalidExeMagic);
        }

        let word = |offset: usize| {
            u32::from_le_bytes([
                data[offset],
                data[offset + 1],
                data[offset + 2],
                data[offset + 3],
            ])
        };

        let header = ExeHeader {
            initial_pc: word(0x10),
            initial_gp: word(0x14),
            destination: word(0x18),
            size: word(0x1C),
            memfill_start: word(0x28),
            memfill_size: word(0x2C),
            stack_base: word(0x30),
            stack_offset: word(0x34),
        };

        // O destino tem que caber na RAM de 2 MB depois de tirar o segmento.
        let physical = crate::memory::physical(header.destination);
        let end = physical as u64 + header.size as u64;
        if physical >= crate::memory::RAM_SIZE as u32 || end > crate::memory::RAM_SIZE as u64 {
            return Err(PsxError::ExeOutOfRange {
                dest: header.destination,
                len: header.size,
            });
        }

        // O corpo pode vir truncado se o arquivo foi cortado; usamos o que há.
        let available = data.len() - HEADER_SIZE;
        let length = (header.size as usize).min(available);
        let body = data[HEADER_SIZE..HEADER_SIZE + length].to_vec();

        Ok(Self { header, body })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Monta um `PS-X EXE` sintético. Nenhum byte vem de software com copyright.
    fn build(destination: u32, size: u32, body: &[u8]) -> Vec<u8> {
        let mut data = vec![0u8; HEADER_SIZE];
        data[0..8].copy_from_slice(b"PS-X EXE");
        data[0x10..0x14].copy_from_slice(&0x8001_0000u32.to_le_bytes());
        data[0x14..0x18].copy_from_slice(&0u32.to_le_bytes());
        data[0x18..0x1C].copy_from_slice(&destination.to_le_bytes());
        data[0x1C..0x20].copy_from_slice(&size.to_le_bytes());
        data[0x30..0x34].copy_from_slice(&0x801F_FF00u32.to_le_bytes());
        data[0x34..0x38].copy_from_slice(&0u32.to_le_bytes());
        data.extend_from_slice(body);
        data
    }

    #[test]
    fn parses_a_valid_header() {
        let data = build(0x8001_0000, 0x800, &[0xAA; 0x800]);
        let exe = Executable::parse(&data).unwrap();
        assert_eq!(exe.header.initial_pc, 0x8001_0000);
        assert_eq!(exe.header.destination, 0x8001_0000);
        assert_eq!(exe.header.size, 0x800);
        assert_eq!(exe.body.len(), 0x800);
        assert_eq!(exe.header.initial_sp(), Some(0x801F_FF00));
    }

    #[test]
    fn rejects_wrong_magic() {
        let mut data = build(0x8001_0000, 0, &[]);
        data[0] = b'X';
        assert_eq!(
            Executable::parse(&data).err(),
            Some(PsxError::InvalidExeMagic)
        );
    }

    #[test]
    fn rejects_truncated_header() {
        assert_eq!(
            Executable::parse(&[0u8; 16]).err(),
            Some(PsxError::TruncatedExe(16))
        );
    }

    #[test]
    fn rejects_destination_outside_ram() {
        let data = build(0x8040_0000, 0x800, &[0; 0x800]);
        assert!(matches!(
            Executable::parse(&data).err(),
            Some(PsxError::ExeOutOfRange { .. })
        ));
    }

    #[test]
    fn rejects_body_that_would_overflow_ram() {
        // Começa perto do fim dos 2 MB e declara mais do que cabe.
        let data = build(0x801F_F000, 0x8000, &[0; 0x800]);
        assert!(matches!(
            Executable::parse(&data).err(),
            Some(PsxError::ExeOutOfRange { .. })
        ));
    }

    #[test]
    fn truncated_body_uses_what_is_available() {
        // Declara 0x1000 bytes mas só entrega 0x100.
        let data = build(0x8001_0000, 0x1000, &[0xBB; 0x100]);
        let exe = Executable::parse(&data).unwrap();
        assert_eq!(exe.body.len(), 0x100);
    }

    #[test]
    fn zero_stack_base_means_keep_the_bios_stack() {
        let mut data = build(0x8001_0000, 0, &[]);
        data[0x30..0x34].copy_from_slice(&0u32.to_le_bytes());
        let exe = Executable::parse(&data).unwrap();
        assert_eq!(exe.header.initial_sp(), None);
    }
}
