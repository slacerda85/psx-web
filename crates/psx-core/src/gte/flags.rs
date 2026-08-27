//! Bits do registrador `FLAG` (`cop2r63`) do GTE.
//!
//! Referência: PSX-SPX — "GTE Registers", tabela de `FLAG`.
//!
//! O agente `@gte` deve setar estes bits em cada saturação. O bit 31 não é
//! setado à mão: ele é derivado da máscara [`Flag::ERROR_MASK`].

/// Um bit de erro/saturação do GTE.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Flag(pub u32);

impl Flag {
    /// `MAC1` estourou para cima (bit 30).
    pub const MAC1_OVERFLOW_POSITIVE: Flag = Flag(1 << 30);
    /// `MAC2` estourou para cima (bit 29).
    pub const MAC2_OVERFLOW_POSITIVE: Flag = Flag(1 << 29);
    /// `MAC3` estourou para cima (bit 28).
    pub const MAC3_OVERFLOW_POSITIVE: Flag = Flag(1 << 28);
    /// `MAC1` estourou para baixo (bit 27).
    pub const MAC1_OVERFLOW_NEGATIVE: Flag = Flag(1 << 27);
    /// `MAC2` estourou para baixo (bit 26).
    pub const MAC2_OVERFLOW_NEGATIVE: Flag = Flag(1 << 26);
    /// `MAC3` estourou para baixo (bit 25).
    pub const MAC3_OVERFLOW_NEGATIVE: Flag = Flag(1 << 25);
    /// `IR1` saturado (bit 24).
    pub const IR1_SATURATED: Flag = Flag(1 << 24);
    /// `IR2` saturado (bit 23).
    pub const IR2_SATURATED: Flag = Flag(1 << 23);
    /// `IR3` saturado (bit 22).
    pub const IR3_SATURATED: Flag = Flag(1 << 22);
    /// Cor vermelha saturada em 0..255 (bit 21).
    pub const COLOR_R_SATURATED: Flag = Flag(1 << 21);
    /// Cor verde saturada (bit 20).
    pub const COLOR_G_SATURATED: Flag = Flag(1 << 20);
    /// Cor azul saturada (bit 19).
    pub const COLOR_B_SATURATED: Flag = Flag(1 << 19);
    /// `SZ3`/`OTZ` saturado em 0..0xFFFF (bit 18).
    pub const SZ3_OTZ_SATURATED: Flag = Flag(1 << 18);
    /// Overflow na divisão — resultado saturado em `0x1FFFF` (bit 17).
    pub const DIVIDE_OVERFLOW: Flag = Flag(1 << 17);
    /// `MAC0` estourou para cima (bit 16).
    pub const MAC0_OVERFLOW_POSITIVE: Flag = Flag(1 << 16);
    /// `MAC0` estourou para baixo (bit 15).
    pub const MAC0_OVERFLOW_NEGATIVE: Flag = Flag(1 << 15);
    /// `SX2` saturado em -1024..1023 (bit 14).
    pub const SX2_SATURATED: Flag = Flag(1 << 14);
    /// `SY2` saturado em -1024..1023 (bit 13).
    pub const SY2_SATURATED: Flag = Flag(1 << 13);
    /// `IR0` saturado em 0..0x1000 (bit 12).
    pub const IR0_SATURATED: Flag = Flag(1 << 12);

    /// Bits que, se ligados, ligam também o bit 31 (erro agregado).
    ///
    /// PSX-SPX: o bit 31 é o OR dos bits **30..23** e **18..13**. A pegadinha
    /// clássica é que `IR1` (24) e `IR2` (23) entram, mas `IR3` (22), as três
    /// saturações de cor (21..19) e `IR0` (12) **não**.
    pub const ERROR_MASK: u32 = 0x7F87_E000;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_mask_includes_the_hard_errors() {
        for flag in [
            Flag::MAC1_OVERFLOW_POSITIVE,
            Flag::MAC3_OVERFLOW_NEGATIVE,
            Flag::SZ3_OTZ_SATURATED,
            Flag::DIVIDE_OVERFLOW,
            Flag::MAC0_OVERFLOW_POSITIVE,
            Flag::SX2_SATURATED,
            Flag::SY2_SATURATED,
            // IR1 e IR2 entram no OR — IR3 não.
            Flag::IR1_SATURATED,
            Flag::IR2_SATURATED,
        ] {
            assert_ne!(flag.0 & Flag::ERROR_MASK, 0, "{flag:?} deve entrar no OR");
        }
    }

    #[test]
    fn error_mask_excludes_ir3_ir0_and_colors() {
        // Quirk: IR3 (bit 22), as cores (21..19) e IR0 (12) não ligam o bit 31,
        // apesar de IR1 (24) e IR2 (23) ligarem.
        for flag in [
            Flag::IR3_SATURATED,
            Flag::IR0_SATURATED,
            Flag::COLOR_R_SATURATED,
            Flag::COLOR_G_SATURATED,
            Flag::COLOR_B_SATURATED,
        ] {
            assert_eq!(flag.0 & Flag::ERROR_MASK, 0, "{flag:?} não entra no OR");
        }
    }
}
