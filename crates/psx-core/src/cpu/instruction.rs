//! Decodificação de instruções MIPS I.
//!
//! Referência: PSX-SPX — "CPU Opcode Encoding".
//!
//! Os três formatos:
//! ```text
//!  R:  000000 ss sss ttttt ddddd aaaaa ffffff   (opcode 0, funct em `ffffff`)
//!  I:  oooooo ss sss ttttt iiiiiiiiiiiiiiii
//!  J:  oooooo tttttttttttttttttttttttttt
//! ```

/// Uma palavra de instrução de 32 bits com os acessores de campo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Instruction(pub u32);

impl Instruction {
    /// Bits 26..31 — opcode primário.
    #[inline(always)]
    pub const fn opcode(self) -> u32 {
        self.0 >> 26
    }

    /// Bits 0..5 — função secundária (formato R).
    #[inline(always)]
    pub const fn funct(self) -> u32 {
        self.0 & 0x3F
    }

    /// Bits 21..25 — registrador fonte.
    #[inline(always)]
    pub const fn rs(self) -> usize {
        ((self.0 >> 21) & 0x1F) as usize
    }

    /// Bits 16..20 — segundo operando / destino em formato I.
    #[inline(always)]
    pub const fn rt(self) -> usize {
        ((self.0 >> 16) & 0x1F) as usize
    }

    /// Bits 11..15 — destino em formato R.
    #[inline(always)]
    pub const fn rd(self) -> usize {
        ((self.0 >> 11) & 0x1F) as usize
    }

    /// Bits 6..10 — quantidade de shift imediato.
    #[inline(always)]
    pub const fn shamt(self) -> u32 {
        (self.0 >> 6) & 0x1F
    }

    /// Bits 0..15 — imediato com zero-extension.
    #[inline(always)]
    pub const fn imm(self) -> u32 {
        self.0 & 0xFFFF
    }

    /// Bits 0..15 — imediato com sign-extension.
    #[inline(always)]
    pub const fn imm_se(self) -> u32 {
        self.0 as i16 as u32
    }

    /// Bits 0..25 — alvo de `J`/`JAL`, ainda sem o shift de 2.
    #[inline(always)]
    pub const fn target(self) -> u32 {
        self.0 & 0x03FF_FFFF
    }

    /// Bits 21..25 quando o opcode é de coprocessador (`MFC`, `MTC`, `CFC`, ...).
    #[inline(always)]
    pub const fn cop_op(self) -> u32 {
        (self.0 >> 21) & 0x1F
    }

    /// Bits 0..24 — comando do GTE em `COP2 imm25`.
    #[inline(always)]
    pub const fn cop2_command(self) -> u32 {
        self.0 & 0x01FF_FFFF
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_r_format() {
        // add $t0, $s1, $s2  =>  000000 10001 10010 01000 00000 100000
        let i = Instruction(0x0232_4020);
        assert_eq!(i.opcode(), 0);
        assert_eq!(i.rs(), 17);
        assert_eq!(i.rt(), 18);
        assert_eq!(i.rd(), 8);
        assert_eq!(i.shamt(), 0);
        assert_eq!(i.funct(), 0x20);
    }

    #[test]
    fn decodes_i_format_with_sign_extension() {
        // addiu $t0, $t1, -1  =>  001001 01001 01000 1111111111111111
        let i = Instruction(0x2528_FFFF);
        assert_eq!(i.opcode(), 0x09);
        assert_eq!(i.rs(), 9);
        assert_eq!(i.rt(), 8);
        assert_eq!(i.imm(), 0xFFFF);
        assert_eq!(i.imm_se(), 0xFFFF_FFFF);
    }

    #[test]
    fn decodes_j_target() {
        // j 0x00100000 => target = 0x00040000
        let i = Instruction(0x0804_0000);
        assert_eq!(i.opcode(), 0x02);
        assert_eq!(i.target(), 0x0004_0000);
    }

    #[test]
    fn decodes_shift_amount() {
        // sll $t0, $t1, 4
        let i = Instruction(0x0009_4100);
        assert_eq!(i.opcode(), 0);
        assert_eq!(i.funct(), 0x00);
        assert_eq!(i.rt(), 9);
        assert_eq!(i.rd(), 8);
        assert_eq!(i.shamt(), 4);
    }
}
