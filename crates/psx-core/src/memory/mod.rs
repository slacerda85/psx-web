//! Memory map e memórias físicas.
//!
//! Referência: PSX-SPX — "Memory Map".

mod ram;

pub use ram::{Ram, RAM_SIZE, SCRATCHPAD_SIZE};

/// Máscara aplicada ao endereço virtual conforme o segmento (bits 31..29).
///
/// PSX-SPX — "Memory Map": KUSEG (`0x0000_0000`) é identidade, KSEG0
/// (`0x8000_0000`, cached) e KSEG1 (`0xA000_0000`, uncached) são espelhos da
/// mesma memória física. KSEG2 (`0xC000_0000`) não é espelho — só contém o
/// registrador de cache control em `0xFFFE_0130`.
const REGION_MASK: [u32; 8] = [
    // KUSEG: 2 GB, sem máscara.
    0xFFFF_FFFF,
    0xFFFF_FFFF,
    0xFFFF_FFFF,
    0xFFFF_FFFF,
    // KSEG0: 512 MB.
    0x7FFF_FFFF,
    // KSEG1: 512 MB.
    0x1FFF_FFFF,
    // KSEG2: 1 GB, sem máscara.
    0xFFFF_FFFF,
    0xFFFF_FFFF,
];

/// Converte um endereço virtual em endereço físico removendo o segmento.
#[inline(always)]
pub const fn physical(addr: u32) -> u32 {
    addr & REGION_MASK[(addr >> 29) as usize]
}

/// Uma faixa contígua do mapa de memória físico.
#[derive(Debug, Clone, Copy)]
pub struct Range(pub u32, pub u32);

impl Range {
    /// Se `addr` cai nesta faixa, devolve o offset a partir da base.
    #[inline(always)]
    pub const fn contains(self, addr: u32) -> Option<u32> {
        let Range(start, length) = self;
        if addr >= start && addr < start + length {
            Some(addr - start)
        } else {
            None
        }
    }
}

/// RAM principal: 2 MB espelhados quatro vezes até 8 MB.
pub const REGION_RAM: Range = Range(0x0000_0000, 8 * 1024 * 1024);
/// Expansion Region 1 — normalmente ausente; leituras devolvem `0xFF`.
pub const REGION_EXPANSION_1: Range = Range(0x1F00_0000, 8 * 1024 * 1024);
/// Scratchpad (D-Cache usada como fast RAM), 1 KB.
pub const REGION_SCRATCHPAD: Range = Range(0x1F80_0000, 1024);
/// Registradores de I/O (4 KB).
pub const REGION_IO: Range = Range(0x1F80_1000, 4 * 1024);
/// Expansion Region 2 — registradores de debug (POST, DIP switches).
pub const REGION_EXPANSION_2: Range = Range(0x1F80_2000, 8 * 1024);
/// Expansion Region 3.
pub const REGION_EXPANSION_3: Range = Range(0x1FA0_0000, 2 * 1024 * 1024);
/// BIOS ROM, 512 KB, somente leitura.
pub const REGION_BIOS: Range = Range(0x1FC0_0000, 512 * 1024);
/// Cache control, único registrador visível em KSEG2.
pub const REGION_CACHE_CONTROL: Range = Range(0xFFFE_0130, 4);

/// Ciclos que um acesso a dados **acrescenta** ao da instrução.
///
/// O barramento do PSX é lento e cada região tem o seu preço: a RAM cobra
/// cerca de 5 ciclos, a scratchpad quase nada, e a SPU quase quarenta. Os
/// valores vêm da medição do `cpu/access-time` do ps1-tests num console real,
/// menos o ciclo que a instrução já custa por si.
///
/// Isso não é refinamento de precisão — é requisito para rodar jogo. A
/// biblioteca da Sony espera VBlank com um prazo contado em **iterações** de
/// um laço, não em tempo; uma CPU que executa toda instrução em um ciclo
/// esgota o prazo antes do VBlank chegar, e a `VSync()` de todo jogo passa a
/// reclamar de timeout.
pub const fn access_cycles(address: u32, width: u8) -> u32 {
    let physical = physical(address);
    // A scratchpad é a cache de dados: chega junto com a instrução.
    if REGION_SCRATCHPAD.contains(physical).is_some() {
        return 0;
    }
    if REGION_RAM.contains(physical).is_some() {
        return 4;
    }
    if REGION_IO.contains(physical).is_some() {
        return io_access_cycles(physical - REGION_IO.0, width);
    }
    if REGION_BIOS.contains(physical).is_some() {
        // ROM de 8 bits: cada byte a mais é outro acesso.
        return match width {
            1 => 6,
            2 => 12,
            _ => 24,
        };
    }
    if REGION_EXPANSION_1.contains(physical).is_some() {
        return match width {
            1 => 6,
            2 => 13,
            _ => 25,
        };
    }
    if REGION_EXPANSION_2.contains(physical).is_some() {
        return match width {
            1 => 10,
            2 => 25,
            _ => 55,
        };
    }
    if REGION_EXPANSION_3.contains(physical).is_some() {
        return match width {
            1 => 6,
            2 => 5,
            _ => 9,
        };
    }
    // Cache control responde na hora.
    0
}

/// Ciclos extras dentro do bloco de I/O, que não tem um preço só.
///
/// O CD-ROM e a SPU ficam atrás de um barramento mais lento que o resto dos
/// registradores, e a diferença é grande demais para ser aproximada por uma
/// média — a SPU sozinha custa mais que dez acessos à RAM.
const fn io_access_cycles(offset: u32, width: u8) -> u32 {
    match offset {
        // CD-ROM: 8, 14 e 26 ciclos conforme a largura.
        0x800..=0x803 => match width {
            1 => 7,
            2 => 13,
            _ => 25,
        },
        // SPU: o periférico mais lento do console.
        0xC00..=0xFFF => match width {
            1 | 2 => 17,
            _ => 38,
        },
        // GPU.
        0x810 | 0x814 => match width {
            1 | 2 => 2,
            _ => 3,
        },
        // O resto do bloco fica entre 2,9 e 3,8 ciclos.
        _ => match width {
            1 | 2 => 3,
            _ => 2,
        },
    }
}

/// A CPU pode buscar instrução deste endereço?
///
/// A scratchpad é alcançável por load e store, mas não pelo barramento de
/// instrução: o console responde com bus error em vez de executar o que
/// estiver lá. O bloco de I/O, ao contrário, **é** buscável — o teste
/// `cpu/code-in-io` confirma que SPU e DMA executam de lá.
pub fn is_executable(address: u32) -> bool {
    REGION_SCRATCHPAD.contains(physical(address)).is_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kseg_mirrors_map_to_same_physical_address() {
        // PSX-SPX: 0x00000000, 0x80000000 e 0xA0000000 são a mesma RAM.
        assert_eq!(physical(0x0000_0000), 0x0000_0000);
        assert_eq!(physical(0x8000_0000), 0x0000_0000);
        assert_eq!(physical(0xA000_0000), 0x0000_0000);

        assert_eq!(physical(0x0000_1234), 0x0000_1234);
        assert_eq!(physical(0x8000_1234), 0x0000_1234);
        assert_eq!(physical(0xA000_1234), 0x0000_1234);
    }

    #[test]
    fn bios_entry_point_maps_to_bios_region() {
        // O reset vector do R3000A é 0xBFC00000 (KSEG1 + 0x1FC00000).
        assert_eq!(physical(0xBFC0_0000), 0x1FC0_0000);
        assert_eq!(REGION_BIOS.contains(physical(0xBFC0_0000)), Some(0));
    }

    #[test]
    fn kseg2_is_not_mirrored() {
        // Cache control precisa continuar em 0xFFFE0130 após a máscara.
        assert_eq!(physical(0xFFFE_0130), 0xFFFE_0130);
        assert_eq!(REGION_CACHE_CONTROL.contains(0xFFFE_0130), Some(0));
    }

    #[test]
    fn range_excludes_upper_bound() {
        let r = Range(0x100, 4);
        assert_eq!(r.contains(0x0FF), None);
        assert_eq!(r.contains(0x100), Some(0));
        assert_eq!(r.contains(0x103), Some(3));
        assert_eq!(r.contains(0x104), None);
    }
}
