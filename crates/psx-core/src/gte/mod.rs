//! GTE — Geometry Transformation Engine (COP2).
//!
//! Referência: PSX-SPX — "Geometry Transformation Engine (GTE)".
//!
//! **Estado atual:** banco de registradores completo (incluindo os espelhos, o
//! FIFO de `SXY`, `IRGB`/`ORGB` e `LZCS`/`LZCR`) e os 22 comandos
//! implementados em [`ops`], com a aritmética de 44 bits, as flags de
//! saturação e a divisão UNR do hardware.
//!
//! Opcodes fora da lista continuam sendo contabilizados em
//! [`Gte::unimplemented_commands`] em vez de falharem em silêncio.

mod flags;
mod ops;

pub use flags::Flag;

/// Estado completo do GTE.
#[derive(Clone)]
pub struct Gte {
    /// `cop2r0..31` — registradores de dados.
    pub(self) data: [u32; 32],
    /// `cop2r32..63` — registradores de controle.
    pub(self) control: [u32; 32],
    /// Comandos recebidos que ainda não têm implementação.
    unimplemented: u64,
    /// Último comando não implementado, para diagnóstico.
    last_unimplemented: u32,
}

impl Gte {
    pub fn new() -> Self {
        Self {
            data: [0; 32],
            control: [0; 32],
            unimplemented: 0,
            last_unimplemented: 0,
        }
    }

    /// Quantos comandos chegaram sem implementação disponível.
    pub const fn unimplemented_commands(&self) -> u64 {
        self.unimplemented
    }

    /// Opcode do último comando não implementado (bits 0..5).
    pub const fn last_unimplemented_command(&self) -> u32 {
        self.last_unimplemented
    }

    // ------------------------------------------------- registradores de dados

    /// `MFC2` / `SWC2` — leitura de `cop2r0..31`.
    pub fn read_data(&self, index: usize) -> u32 {
        match index {
            // VZ0, VZ1 e VZ2 são signed de 16 bits: o hardware só tem essas
            // 16 linhas, e o que sobra da escrita não volta na leitura.
            1 | 3 | 5 => self.data[index] as i16 as u32,
            // OTZ e SZ0..SZ3 são unsigned de 16 bits.
            7 | 16..=19 => self.data[index] & 0xFFFF,
            // IR0..IR3 são signed de 16 bits.
            8..=11 => self.data[index] as i16 as u32,
            // SXYP é um espelho de leitura de SXY2.
            15 => self.data[14],
            // IRGB e ORGB devolvem a cor comprimida derivada de IR1..IR3.
            28 | 29 => {
                let r = (self.data[9] as i16 >> 7).clamp(0, 0x1F) as u32;
                let g = (self.data[10] as i16 >> 7).clamp(0, 0x1F) as u32;
                let b = (self.data[11] as i16 >> 7).clamp(0, 0x1F) as u32;
                r | (g << 5) | (b << 10)
            }
            // LZCR: contagem de bits iguais ao sinal no topo de LZCS.
            31 => leading_bit_count(self.data[30]),
            _ => self.data[index],
        }
    }

    /// `MTC2` / `LWC2` — escrita em `cop2r0..31`.
    ///
    /// Os registradores de 16 bits são truncados **na escrita**, e não
    /// mascarados na leitura: o hardware simplesmente não tem as outras 16
    /// linhas. Mascarar só na leitura deixaria o lixo visível para o resto do
    /// core — foi assim que o `AVSZ3` somava as partes altas dos `SZ`.
    pub fn write_data(&mut self, index: usize, value: u32) {
        match index {
            // VZ0..VZ2 e IR0..IR3: 16 bits com sinal.
            1 | 3 | 5 | 8..=11 => self.data[index] = value as i16 as u32,
            // OTZ e SZ0..SZ3: 16 bits sem sinal.
            7 | 16..=19 => self.data[index] = value & 0xFFFF,
            // Escrever em SXYP empurra o FIFO de coordenadas de tela.
            15 => {
                self.data[12] = self.data[13];
                self.data[13] = self.data[14];
                self.data[14] = value;
            }
            // IRGB descomprime para IR1..IR3.
            28 => {
                self.data[28] = value & 0x7FFF;
                self.data[9] = (value & 0x1F) * 0x80;
                self.data[10] = ((value >> 5) & 0x1F) * 0x80;
                self.data[11] = ((value >> 10) & 0x1F) * 0x80;
            }
            // ORGB e LZCR são somente leitura.
            29 | 31 => {}
            _ => self.data[index] = value,
        }
    }

    // ---------------------------------------------- registradores de controle

    /// `CFC2` — leitura de `cop2r32..63`.
    pub fn read_control(&self, index: usize) -> u32 {
        match index {
            // Registradores de 16 bits com sinal: os cantos das três matrizes
            // (RT33, L33, LB3), DQA, ZSF3 e ZSF4.
            //
            // H (c26) entra na lista por um quirk documentado: é usado como
            // unsigned na divisão, mas lido com o sinal estendido.
            4 | 12 | 20 | 26 | 27 | 29 | 30 => self.control[index] as i16 as u32,
            31 => self.flag(),
            _ => self.control[index],
        }
    }

    /// `CTC2` — escrita em `cop2r32..63`.
    pub fn write_control(&mut self, index: usize, value: u32) {
        match index {
            // Cantos das matrizes, DQA, ZSF3 e ZSF4: 16 bits com sinal.
            4 | 12 | 20 | 27 | 29 | 30 => self.control[index] = value as i16 as u32,
            // H é usado como unsigned na divisão, mesmo sendo lido com sinal.
            26 => self.control[26] = value & 0xFFFF,
            31 => self.control[31] = value & 0x7FFF_F000,
            _ => self.control[index] = value,
        }
    }

    /// `FLAG` (`cop2r63`) com o bit 31 recalculado.
    ///
    /// PSX-SPX: o bit 31 é o OR de todos os bits de erro "importantes"
    /// (máscara `0x7F87_E000`), e não é gravável diretamente.
    fn flag(&self) -> u32 {
        let raw = self.control[31] & 0x7FFF_F000;
        if raw & 0x7F87_E000 != 0 {
            raw | 0x8000_0000
        } else {
            raw
        }
    }

    /// Executa um comando `COP2 imm25`. Devolve os ciclos consumidos.
    pub fn execute(&mut self, command: u32) -> u32 {
        let decoded = ops::Command::decode(command);
        // Todo comando começa zerando FLAG.
        self.control[31] = 0;

        if !self.dispatch(decoded) {
            self.unimplemented += 1;
            self.last_unimplemented = decoded.opcode;
        }

        command_cycles(decoded.opcode)
    }
}

impl Default for Gte {
    fn default() -> Self {
        Self::new()
    }
}

/// Conta os bits no topo de `value` iguais ao bit de sinal.
///
/// PSX-SPX: `LZCR` devolve a contagem de *uns* à esquerda para valores
/// negativos e de *zeros* à esquerda para positivos. O resultado vai de 1 a 32.
fn leading_bit_count(value: u32) -> u32 {
    if value & 0x8000_0000 != 0 {
        value.leading_ones()
    } else {
        value.leading_zeros()
    }
}

/// Ciclos por comando do GTE (PSX-SPX — "GTE Opcode Summary").
const fn command_cycles(opcode: u32) -> u32 {
    match opcode {
        0x01 => 15, // RTPS
        0x06 => 8,  // NCLIP
        0x0C => 6,  // OP
        0x10 => 8,  // DPCS
        0x11 => 8,  // INTPL
        0x12 => 8,  // MVMVA
        0x13 => 19, // NCDS
        0x14 => 13, // CDP
        0x16 => 44, // NCDT
        0x1B => 17, // NCCS
        0x1C => 11, // CC
        0x1E => 14, // NCS
        0x20 => 30, // NCT
        0x28 => 5,  // SQR
        0x29 => 8,  // DCPL
        0x2A => 17, // DPCT
        0x2D => 5,  // AVSZ3
        0x2E => 6,  // AVSZ4
        0x30 => 23, // RTPT
        0x3D => 5,  // GPF
        0x3E => 5,  // GPL
        0x3F => 39, // NCCT
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sxyp_mirrors_sxy2_on_read() {
        let mut gte = Gte::new();
        gte.write_data(14, 0x1234_5678);
        assert_eq!(gte.read_data(15), 0x1234_5678);
    }

    #[test]
    fn writing_sxyp_pushes_the_screen_fifo() {
        let mut gte = Gte::new();
        gte.write_data(12, 0xAAAA_AAAA); // SXY0
        gte.write_data(13, 0xBBBB_BBBB); // SXY1
        gte.write_data(14, 0xCCCC_CCCC); // SXY2

        gte.write_data(15, 0xDDDD_DDDD); // push

        assert_eq!(gte.read_data(12), 0xBBBB_BBBB);
        assert_eq!(gte.read_data(13), 0xCCCC_CCCC);
        assert_eq!(gte.read_data(14), 0xDDDD_DDDD);
    }

    #[test]
    fn ir_registers_read_sign_extended() {
        let mut gte = Gte::new();
        gte.write_data(9, 0x0000_8000);
        assert_eq!(gte.read_data(9), 0xFFFF_8000);
    }

    #[test]
    fn sz_registers_read_unsigned() {
        let mut gte = Gte::new();
        gte.write_data(16, 0xFFFF_8000);
        assert_eq!(gte.read_data(16), 0x0000_8000);
    }

    #[test]
    fn irgb_round_trips_through_ir123() {
        let mut gte = Gte::new();
        // r=1, g=2, b=3
        gte.write_data(28, 1 | (2 << 5) | (3 << 10));
        assert_eq!(gte.read_data(9), 0x0080);
        assert_eq!(gte.read_data(10), 0x0100);
        assert_eq!(gte.read_data(11), 0x0180);
        assert_eq!(gte.read_data(28), 1 | (2 << 5) | (3 << 10));
        // ORGB devolve o mesmo valor.
        assert_eq!(gte.read_data(29), gte.read_data(28));
    }

    #[test]
    fn orgb_and_lzcr_are_read_only() {
        let mut gte = Gte::new();
        gte.write_data(29, 0xFFFF_FFFF);
        gte.write_data(31, 0xFFFF_FFFF);
        assert_eq!(gte.read_data(29), 0);
        assert_eq!(gte.read_data(31), 32, "LZCR de LZCS=0 é 32 zeros");
    }

    #[test]
    fn lzcr_counts_sign_bits() {
        let mut gte = Gte::new();
        gte.write_data(30, 0x0000_0000);
        assert_eq!(gte.read_data(31), 32);
        gte.write_data(30, 0xFFFF_FFFF);
        assert_eq!(gte.read_data(31), 32);
        gte.write_data(30, 0x0000_FFFF);
        assert_eq!(gte.read_data(31), 16);
        gte.write_data(30, 0xFFFF_0000);
        assert_eq!(gte.read_data(31), 16);
        gte.write_data(30, 0x8000_0000);
        assert_eq!(gte.read_data(31), 1);
    }

    #[test]
    fn h_is_read_sign_extended() {
        let mut gte = Gte::new();
        gte.write_control(26, 0x0000_FFFF);
        assert_eq!(gte.read_control(26), 0xFFFF_FFFF);
    }

    #[test]
    fn flag_bit31_is_derived_not_stored() {
        let mut gte = Gte::new();
        // Bit 13 pertence à máscara de erro agregado.
        gte.write_control(31, 1 << 13);
        assert_eq!(gte.read_control(31) & 0x8000_0000, 0x8000_0000);

        // Bit 12 não pertence.
        gte.write_control(31, 1 << 12);
        assert_eq!(gte.read_control(31) & 0x8000_0000, 0);
    }

    #[test]
    fn a_known_command_is_not_counted_as_unimplemented() {
        let mut gte = Gte::new();
        let cycles = gte.execute(0x0018_0001); // RTPS
        assert_eq!(cycles, 15);
        assert_eq!(gte.unimplemented_commands(), 0);
    }

    #[test]
    fn an_unknown_opcode_is_counted_instead_of_ignored() {
        let mut gte = Gte::new();
        gte.execute(0x0000_0002); // opcode 0x02 não existe
        assert_eq!(gte.unimplemented_commands(), 1);
        assert_eq!(gte.last_unimplemented_command(), 0x02);
    }

    #[test]
    fn every_command_clears_flag_before_running() {
        let mut gte = Gte::new();
        gte.write_control(31, 1 << 13);
        // NCLIP com o FIFO zerado não levanta nenhuma flag.
        gte.execute(0x0000_0006);
        assert_eq!(gte.read_control(31), 0, "FLAG é zerada a cada comando");
    }
}
