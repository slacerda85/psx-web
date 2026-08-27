//! Os comandos do GTE.
//!
//! Referência: PSX-SPX — "GTE Opcode Summary", "GTE Coordinate Calculation
//! Commands", "GTE General Purpose Calculation Commands", "GTE Color
//! Calculation Commands", "GTE Division Inaccuracy".
//!
//! Toda a aritmética é fixed-point sobre inteiros. Os acumuladores `MAC1..3`
//! são de **44 bits** com sinal, e não 32: o overflow é detectado nessa
//! largura e só depois o valor é truncado para o registrador de 32 bits. Fazer
//! a conta em `i64` e checar contra os limites de 44 bits é o que reproduz as
//! flags do hardware.

use super::{Flag, Gte};

/// Índice de `MAC0` no banco de dados.
const MAC0: usize = 24;
/// Índice de `MAC1`; `MAC2` e `MAC3` seguem.
const MAC1: usize = 25;
/// Índice de `IR0`; `IR1..IR3` seguem.
const IR0: usize = 8;

/// Maior valor representável em 44 bits com sinal.
const MAC_MAX: i64 = 0x7FF_FFFF_FFFF;
/// Menor valor representável em 44 bits com sinal.
const MAC_MIN: i64 = -0x800_0000_0000;

/// Campos do `COP2 imm25` que mudam como o comando se comporta.
#[derive(Debug, Clone, Copy)]
pub(super) struct Command {
    pub opcode: u32,
    /// Deslocamento aplicado a `MAC1..3`: 12 quando o bit `sf` está ligado.
    pub sf: u32,
    /// `lm` — satura `IR1..3` em `0..0x7FFF` em vez de `-0x8000..0x7FFF`.
    pub lm: bool,
    /// Matriz escolhida pelo `MVMVA` (bits 17..18).
    pub mx: u32,
    /// Vetor escolhido pelo `MVMVA` (bits 15..16).
    pub v: u32,
    /// Vetor de translação escolhido pelo `MVMVA` (bits 13..14).
    pub cv: u32,
}

impl Command {
    pub fn decode(word: u32) -> Self {
        Self {
            opcode: word & 0x3F,
            sf: if word & (1 << 19) != 0 { 12 } else { 0 },
            lm: word & (1 << 10) != 0,
            mx: (word >> 17) & 3,
            v: (word >> 15) & 3,
            cv: (word >> 13) & 3,
        }
    }
}

/// Tabela de recíprocos usada pela divisão UNR do GTE.
///
/// PSX-SPX — "GTE Division Inaccuracy": o hardware não divide, ele estima o
/// recíproco por Newton-Raphson a partir desta tabela. Reproduzir a tabela é
/// obrigatório: uma divisão exata dá resultados diferentes do console em
/// vários pixels, e jogos que comparam Z contra limiares se comportam
/// diferente.
const UNR_TABLE: [u8; 257] = {
    let mut table = [0u8; 257];
    let mut index = 0;
    while index < 257 {
        let value = (0x4_0000 / (index as i32 + 0x100) + 1) / 2 - 0x101;
        table[index] = if value < 0 { 0 } else { value as u8 };
        index += 1;
    }
    table
};

impl Gte {
    pub(super) fn set_flag(&mut self, flag: Flag) {
        self.control[31] |= flag.0;
    }

    // ------------------------------------------------------------ acumuladores

    /// Checa `value` contra a faixa de 44 bits e devolve-o truncado nela.
    fn check_mac(&mut self, which: usize, value: i64) -> i64 {
        if value > MAC_MAX {
            self.set_flag(match which {
                1 => Flag::MAC1_OVERFLOW_POSITIVE,
                2 => Flag::MAC2_OVERFLOW_POSITIVE,
                _ => Flag::MAC3_OVERFLOW_POSITIVE,
            });
        } else if value < MAC_MIN {
            self.set_flag(match which {
                1 => Flag::MAC1_OVERFLOW_NEGATIVE,
                2 => Flag::MAC2_OVERFLOW_NEGATIVE,
                _ => Flag::MAC3_OVERFLOW_NEGATIVE,
            });
        }
        // Sign-extend de 44 bits: o acumulador dá a volta, não satura.
        (value << 20) >> 20
    }

    /// Escreve `MAC0` checando o overflow de 32 bits.
    fn set_mac0(&mut self, value: i64) -> i32 {
        if value > 0x7FFF_FFFF {
            self.set_flag(Flag::MAC0_OVERFLOW_POSITIVE);
        } else if value < -0x8000_0000 {
            self.set_flag(Flag::MAC0_OVERFLOW_NEGATIVE);
        }
        self.data[MAC0] = value as u32;
        value as i32
    }

    /// Escreve `IR1..3` saturando conforme `lm`.
    fn set_ir(&mut self, which: usize, value: i32, lm: bool) {
        let min = if lm { 0 } else { -0x8000 };
        if value < min || value > 0x7FFF {
            self.set_flag(match which {
                1 => Flag::IR1_SATURATED,
                2 => Flag::IR2_SATURATED,
                _ => Flag::IR3_SATURATED,
            });
        }
        self.data[IR0 + which] = value.clamp(min, 0x7FFF) as u32;
    }

    /// Calcula `MAC1..3` a partir de três somas de 44 bits e copia para `IR`.
    fn set_mac_and_ir(&mut self, values: [i64; 3], sf: u32, lm: bool) {
        let mut checked = [0i64; 3];
        for (index, &value) in values.iter().enumerate() {
            checked[index] = self.check_mac(index + 1, value);
        }
        self.store_mac_and_ir(checked, sf, lm);
    }

    /// Guarda valores **já checados** em `MAC1..3` e propaga para `IR1..3`.
    fn store_mac_and_ir(&mut self, values: [i64; 3], sf: u32, lm: bool) {
        let mut macs = [0i32; 3];
        for (index, &value) in values.iter().enumerate() {
            macs[index] = (value >> sf) as i32;
            self.data[MAC1 + index] = macs[index] as u32;
        }
        for (index, &mac) in macs.iter().enumerate() {
            self.set_ir(index + 1, mac, lm);
        }
    }

    /// Acumula `base + Σ matrix_row[i] * vector[i]`, checando o overflow de 44
    /// bits **a cada produto somado**.
    ///
    /// O hardware é um multiplicador-acumulador de três estágios, e a flag é
    /// avaliada em cada estágio. A diferença aparece quando a soma estoura para
    /// cima num termo e volta para baixo no seguinte: o resultado final é o
    /// mesmo que somar tudo de uma vez, mas as duas flags de overflow ficam
    /// acesas. Somar em bloco e checar no fim não acende nenhuma.
    fn multiply_accumulate(
        &mut self,
        row: usize,
        base: i64,
        matrix_row: [i64; 3],
        vector: [i64; 3],
    ) -> i64 {
        let mut accumulator = base;
        for (factor, coordinate) in matrix_row.iter().zip(vector.iter()) {
            accumulator = self.check_mac(row + 1, accumulator + factor * coordinate);
        }
        accumulator
    }

    // ------------------------------------------------------------------ FIFOs

    fn push_sz(&mut self, value: i64) {
        self.data[16] = self.data[17];
        self.data[17] = self.data[18];
        self.data[18] = self.data[19];
        if !(0..=0xFFFF).contains(&value) {
            self.set_flag(Flag::SZ3_OTZ_SATURATED);
        }
        self.data[19] = value.clamp(0, 0xFFFF) as u32;
    }

    fn push_sxy(&mut self, x: i64, y: i64) {
        if !(-0x400..=0x3FF).contains(&x) {
            self.set_flag(Flag::SX2_SATURATED);
        }
        if !(-0x400..=0x3FF).contains(&y) {
            self.set_flag(Flag::SY2_SATURATED);
        }
        let x = x.clamp(-0x400, 0x3FF) as i16 as u16 as u32;
        let y = y.clamp(-0x400, 0x3FF) as i16 as u16 as u32;
        self.data[12] = self.data[13];
        self.data[13] = self.data[14];
        self.data[14] = x | (y << 16);
    }

    /// Empurra a cor derivada de `MAC1..3`, saturando cada canal em 0..255.
    fn push_color(&mut self) {
        let code = self.data[6] & 0xFF00_0000;
        let mut channels = [0u32; 3];
        for (index, channel) in channels.iter_mut().enumerate() {
            let value = (self.data[MAC1 + index] as i32) >> 4;
            if !(0..=0xFF).contains(&value) {
                self.set_flag(match index {
                    0 => Flag::COLOR_R_SATURATED,
                    1 => Flag::COLOR_G_SATURATED,
                    _ => Flag::COLOR_B_SATURATED,
                });
            }
            *channel = value.clamp(0, 0xFF) as u32;
        }
        self.data[20] = self.data[21];
        self.data[21] = self.data[22];
        self.data[22] = channels[0] | (channels[1] << 8) | (channels[2] << 16) | code;
    }

    // -------------------------------------------------------------- acessores

    /// Uma das três matrizes de controle, desempacotada de meias-palavras.
    fn matrix(&self, base: usize) -> [[i64; 3]; 3] {
        let low = |word: u32| word as i16 as i64;
        let high = |word: u32| (word >> 16) as i16 as i64;
        let c = |offset: usize| self.control[base + offset];
        [
            [low(c(0)), high(c(0)), low(c(1))],
            [high(c(1)), low(c(2)), high(c(2))],
            [low(c(3)), high(c(3)), low(c(4))],
        ]
    }

    /// A matriz "lixo" que o `MVMVA` usa quando `mx = 3`.
    ///
    /// Não é um erro nosso: o decodificador do hardware não trata esse valor e
    /// acaba lendo pedaços de `RGBC`, `IR0` e da matriz de rotação. Alguns
    /// jogos dependem disso sem saber.
    fn garbage_matrix(&self) -> [[i64; 3]; 3] {
        let red = ((self.data[6] & 0xFF) << 4) as i64;
        let ir0 = self.data[IR0] as i16 as i64;
        let rt13 = (self.control[1] & 0xFFFF) as u16 as i16 as i64;
        let rt22 = (self.control[2] & 0xFFFF) as u16 as i16 as i64;
        [[-red, red, ir0], [rt13, rt13, rt13], [rt22, rt22, rt22]]
    }

    /// Um dos três vetores de entrada (`V0`, `V1`, `V2`).
    fn vector(&self, index: usize) -> [i64; 3] {
        let packed = self.data[index * 2];
        [
            packed as i16 as i64,
            (packed >> 16) as i16 as i64,
            self.data[index * 2 + 1] as i16 as i64,
        ]
    }

    /// `IR1..IR3` como vetor, que é a quarta opção de entrada do `MVMVA`.
    fn ir_vector(&self) -> [i64; 3] {
        [
            self.data[9] as i16 as i64,
            self.data[10] as i16 as i64,
            self.data[11] as i16 as i64,
        ]
    }

    /// Os três canais de `RGBC`, já escalados para a faixa dos acumuladores.
    fn rgb(&self) -> [i64; 3] {
        [
            (self.data[6] & 0xFF) as i64,
            ((self.data[6] >> 8) & 0xFF) as i64,
            ((self.data[6] >> 16) & 0xFF) as i64,
        ]
    }

    fn translation(&self) -> [i64; 3] {
        [
            self.control[5] as i32 as i64,
            self.control[6] as i32 as i64,
            self.control[7] as i32 as i64,
        ]
    }

    fn background(&self) -> [i64; 3] {
        [
            self.control[13] as i32 as i64,
            self.control[14] as i32 as i64,
            self.control[15] as i32 as i64,
        ]
    }

    fn far_color(&self) -> [i64; 3] {
        [
            self.control[21] as i32 as i64,
            self.control[22] as i32 as i64,
            self.control[23] as i32 as i64,
        ]
    }

    // -------------------------------------------------------------- divisão

    /// Divisão UNR: `(((H * 0x20000) / SZ3) + 1) / 2`, saturada em `0x1FFFF`.
    fn divide(&mut self, numerator: u32, denominator: u32) -> u32 {
        // Sem essa guarda a estimativa diverge; o hardware satura e marca.
        if denominator == 0 || numerator >= denominator * 2 {
            self.set_flag(Flag::DIVIDE_OVERFLOW);
            return 0x1FFFF;
        }

        let shift = (denominator as u16).leading_zeros();
        let numerator = (numerator as u64) << shift;
        let denominator = (denominator as u64) << shift;

        // Duas iterações de Newton-Raphson a partir da tabela.
        let index = ((denominator - 0x7FC0) >> 7) as usize;
        let factor = UNR_TABLE[index] as u64 + 0x101;
        let factor = ((0x0200_0080 - (denominator * factor)) >> 8) * factor;
        let factor = (0x0000_0080 + factor) >> 8;

        let result = ((numerator * factor) + 0x8000) >> 16;
        result.min(0x1FFFF) as u32
    }

    // ------------------------------------------------------------- comandos

    /// Executa um comando decodificado. Devolve `false` para opcode inválido.
    pub(super) fn dispatch(&mut self, command: Command) -> bool {
        match command.opcode {
            0x01 => self.rtps(0, command, true),
            0x06 => self.nclip(),
            0x0C => self.op(command),
            0x10 => self.dpcs(command, false),
            0x11 => self.intpl(command),
            0x12 => self.mvmva(command),
            0x13 => self.ncds(0, command),
            0x14 => self.cdp(command),
            0x16 => {
                for vector in 0..3 {
                    self.ncds(vector, command);
                }
            }
            0x1B => self.nccs(0, command),
            0x1C => self.cc(command),
            0x1E => self.ncs(0, command),
            0x20 => {
                for vector in 0..3 {
                    self.ncs(vector, command);
                }
            }
            0x28 => self.sqr(command),
            0x29 => self.dcpl(command),
            0x2A => self.dpcs(command, true),
            0x2D => self.avsz3(),
            0x2E => self.avsz4(),
            0x30 => {
                // Só a última das três projeções alimenta IR0 e a profundidade.
                for vector in 0..3 {
                    self.rtps(vector, command, vector == 2);
                }
            }
            0x3D => self.gpf(command),
            0x3E => self.gpl(command),
            0x3F => {
                for vector in 0..3 {
                    self.nccs(vector, command);
                }
            }
            _ => return false,
        }
        true
    }

    /// `RTPS` / `RTPT` — projeta um vértice em coordenadas de tela.
    fn rtps(&mut self, vector: usize, command: Command, last: bool) {
        let Command { sf, lm, .. } = command;
        let rt = self.matrix(0);
        let tr = self.translation();
        let v = self.vector(vector);

        let mut macs = [0i32; 3];
        let mut mac3_full = 0i64;
        for row in 0..3 {
            let checked = self.multiply_accumulate(row, tr[row] * 0x1000, rt[row], v);
            if row == 2 {
                mac3_full = checked;
            }
            macs[row] = (checked >> sf) as i32;
            self.data[MAC1 + row] = macs[row] as u32;
        }

        self.set_ir(1, macs[0], lm);
        self.set_ir(2, macs[1], lm);
        // Quirk do RTPS: a flag de IR3 olha o MAC3 deslocado por 12 fixos,
        // independentemente de `sf`, mas o valor guardado usa o MAC3 pós-`sf`.
        // Com sf=0 os dois divergem, e é aí que o hardware surpreende.
        let for_flag = (mac3_full >> 12) as i32;
        if !(-0x8000..=0x7FFF).contains(&for_flag) {
            self.set_flag(Flag::IR3_SATURATED);
        }
        let min = if lm { 0 } else { -0x8000 };
        self.data[11] = macs[2].clamp(min, 0x7FFF) as u32;

        // SZ3 vem sempre do MAC3 sem `sf`, deslocado por 12.
        self.push_sz(mac3_full >> 12);

        let sz3 = self.data[19];
        let h = self.control[26] & 0xFFFF;
        let quotient = self.divide(h, sz3) as i64;

        let ofx = self.control[24] as i32 as i64;
        let ofy = self.control[25] as i32 as i64;
        let ir1 = self.data[9] as i16 as i64;
        let ir2 = self.data[10] as i16 as i64;

        // A coordenada de tela sai do resultado **inteiro**, não do MAC0 já
        // truncado em 32 bits. O registrador guarda a versão truncada e marca
        // o overflow, mas a saturação em -1024..1023 olha o valor completo —
        // com o truncado, um resultado grande e positivo vira um número
        // negativo pequeno e a saturação nem chega a ser detectada.
        let screen_x = quotient * ir1 + ofx;
        self.set_mac0(screen_x);
        let screen_y = quotient * ir2 + ofy;
        self.set_mac0(screen_y);
        self.push_sxy(screen_x >> 16, screen_y >> 16);

        if last {
            let dqa = self.control[27] as i16 as i64;
            let dqb = self.control[28] as i32 as i64;
            let depth = quotient * dqa + dqb;
            self.set_mac0(depth);
            self.set_ir0((depth >> 12).clamp(i32::MIN as i64, i32::MAX as i64) as i32);
        }
    }

    /// `IR0` satura em `0..0x1000`, faixa diferente da de `IR1..3`.
    fn set_ir0(&mut self, value: i32) {
        if !(0..=0x1000).contains(&value) {
            self.set_flag(Flag::IR0_SATURATED);
        }
        self.data[IR0] = value.clamp(0, 0x1000) as u32;
    }

    /// `NCLIP` — área com sinal do triângulo na tela, para descartar costas.
    fn nclip(&mut self) {
        let sx = |packed: u32| packed as i16 as i64;
        let sy = |packed: u32| (packed >> 16) as i16 as i64;
        let (s0, s1, s2) = (self.data[12], self.data[13], self.data[14]);

        let area = sx(s0) * sy(s1) + sx(s1) * sy(s2) + sx(s2) * sy(s0)
            - sx(s0) * sy(s2)
            - sx(s1) * sy(s0)
            - sx(s2) * sy(s1);
        self.set_mac0(area);
    }

    /// `OP` — produto vetorial entre `IR` e a diagonal da matriz de rotação.
    fn op(&mut self, command: Command) {
        let d1 = (self.control[0] & 0xFFFF) as u16 as i16 as i64;
        let d2 = (self.control[2] & 0xFFFF) as u16 as i16 as i64;
        let d3 = self.control[4] as i16 as i64;
        let [ir1, ir2, ir3] = self.ir_vector();

        self.set_mac_and_ir(
            [
                ir3 * d2 - ir2 * d3,
                ir1 * d3 - ir3 * d1,
                ir2 * d1 - ir1 * d2,
            ],
            command.sf,
            command.lm,
        );
    }

    /// `SQR` — quadrado de `IR1..3`. O resultado nunca é negativo, então `lm`
    /// não muda nada aqui.
    fn sqr(&mut self, command: Command) {
        let [ir1, ir2, ir3] = self.ir_vector();
        self.set_mac_and_ir([ir1 * ir1, ir2 * ir2, ir3 * ir3], command.sf, command.lm);
    }

    /// `AVSZ3` — média ponderada de `SZ1..SZ3`, usada para ordenar polígonos.
    fn avsz3(&mut self) {
        let zsf3 = self.control[29] as i16 as i64;
        let sum = self.data[17] as i64 + self.data[18] as i64 + self.data[19] as i64;
        // Como no RTPS, a saturação olha o resultado inteiro: o MAC0 truncado
        // em 32 bits pode ter trocado de sinal e saturaria para o lado errado.
        let full = zsf3 * sum;
        self.set_mac0(full);
        self.set_otz(full >> 12);
    }

    /// `AVSZ4` — média ponderada de `SZ0..SZ3`.
    fn avsz4(&mut self) {
        let zsf4 = self.control[30] as i16 as i64;
        let sum = self.data[16] as i64
            + self.data[17] as i64
            + self.data[18] as i64
            + self.data[19] as i64;
        let full = zsf4 * sum;
        self.set_mac0(full);
        self.set_otz(full >> 12);
    }

    fn set_otz(&mut self, value: i64) {
        if !(0..=0xFFFF).contains(&value) {
            self.set_flag(Flag::SZ3_OTZ_SATURATED);
        }
        self.data[7] = value.clamp(0, 0xFFFF) as u32;
    }

    /// `MVMVA` — multiplicação matriz-vetor genérica.
    fn mvmva(&mut self, command: Command) {
        let matrix = match command.mx {
            0 => self.matrix(0),
            1 => self.matrix(8),
            2 => self.matrix(16),
            _ => self.garbage_matrix(),
        };
        let vector = match command.v {
            0 => self.vector(0),
            1 => self.vector(1),
            2 => self.vector(2),
            _ => self.ir_vector(),
        };

        match command.cv {
            0 => {
                let tr = self.translation();
                self.multiply_add(matrix, vector, tr, command);
            }
            1 => {
                let bk = self.background();
                self.multiply_add(matrix, vector, bk, command);
            }
            2 => self.multiply_add_far_color_bug(matrix, vector, command),
            _ => self.multiply_add(matrix, vector, [0; 3], command),
        }
    }

    fn multiply_add(
        &mut self,
        matrix: [[i64; 3]; 3],
        vector: [i64; 3],
        translation: [i64; 3],
        command: Command,
    ) {
        let mut sums = [0i64; 3];
        for (row, sum) in sums.iter_mut().enumerate() {
            *sum = translation[row] * 0x1000;
        }
        for row in 0..3 {
            sums[row] = self.multiply_accumulate(row, sums[row], matrix[row], vector);
        }
        self.store_mac_and_ir(sums, command.sf, command.lm);
    }

    /// `MVMVA` com `cv = 2` (far color) é **bugado no hardware**.
    ///
    /// PSX-SPX: o somador satura no primeiro produto e perde a contribuição
    /// dele; o resultado fica com `FC*0x1000 + m[row][0]*v[0]` jogado fora,
    /// sobrando só os dois últimos produtos. As flags, porém, são levantadas
    /// pelo cálculo completo — por isso o descarte é feito passando pelo
    /// caminho normal de `IR` antes de recomeçar a soma.
    fn multiply_add_far_color_bug(
        &mut self,
        matrix: [[i64; 3]; 3],
        vector: [i64; 3],
        command: Command,
    ) {
        let fc = self.far_color();
        let mut sums = [0i64; 3];

        for row in 0..3 {
            let discarded = fc[row] * 0x1000 + matrix[row][0] * vector[0];
            let checked = self.check_mac(row + 1, discarded);
            // lm é forçado a falso neste passo intermediário.
            self.set_ir(row + 1, (checked >> command.sf) as i32, false);
            sums[row] = matrix[row][1] * vector[1] + matrix[row][2] * vector[2];
        }

        self.set_mac_and_ir(sums, command.sf, command.lm);
    }

    /// Interpolação em direção à far color, compartilhada por vários comandos.
    ///
    /// `[MAC] = MAC + (FC - MAC) * IR0`, com o detalhe de que o passo
    /// intermediário grava `IR` com `lm` desligado.
    fn interpolate(&mut self, command: Command) {
        let fc = self.far_color();
        let mut deltas = [0i64; 3];
        for (row, delta) in deltas.iter_mut().enumerate() {
            let mac = self.data[MAC1 + row] as i32 as i64;
            *delta = (fc[row] << 12) - mac;
        }
        for (row, &delta) in deltas.iter().enumerate() {
            let checked = self.check_mac(row + 1, delta);
            self.set_ir(row + 1, (checked >> command.sf) as i32, false);
        }

        let ir0 = self.data[IR0] as i16 as i64;
        let mut sums = [0i64; 3];
        for (row, sum) in sums.iter_mut().enumerate() {
            let ir = self.data[IR0 + 1 + row] as i16 as i64;
            let mac = self.data[MAC1 + row] as i32 as i64;
            *sum = ir * ir0 + mac;
        }
        self.set_mac_and_ir(sums, command.sf, command.lm);
    }

    /// `DPCS` (uma vez) e `DPCT` (três vezes) — depth cueing sobre a cor.
    fn dpcs(&mut self, command: Command, from_fifo: bool) {
        let repeats = if from_fifo { 3 } else { 1 };
        for _ in 0..repeats {
            let source = if from_fifo {
                // DPCT consome sempre RGB0, que o push do fim vai rotacionar.
                [
                    (self.data[20] & 0xFF) as i64,
                    ((self.data[20] >> 8) & 0xFF) as i64,
                    ((self.data[20] >> 16) & 0xFF) as i64,
                ]
            } else {
                self.rgb()
            };
            for (row, &channel) in source.iter().enumerate() {
                self.data[MAC1 + row] = ((channel << 16) as i32) as u32;
            }
            self.interpolate(command);
            self.push_color();
        }
    }

    /// `INTPL` — interpola `IR` em direção à far color.
    fn intpl(&mut self, command: Command) {
        for (row, &value) in self.ir_vector().iter().enumerate() {
            self.data[MAC1 + row] = ((value << 12) as i32) as u32;
        }
        self.interpolate(command);
        self.push_color();
    }

    /// `DCPL` — aplica a cor do vértice a `IR` e interpola.
    fn dcpl(&mut self, command: Command) {
        let rgb = self.rgb();
        let ir = self.ir_vector();
        for row in 0..3 {
            self.data[MAC1 + row] = ((rgb[row] * ir[row]) << 4) as i32 as u32;
        }
        self.interpolate(command);
        self.push_color();
    }

    /// Passo comum da família de iluminação: `IR = LLM * V`, depois
    /// `IR = BK * 0x1000 + LCM * IR`.
    fn light(&mut self, vector: usize, command: Command) {
        let llm = self.matrix(8);
        let v = self.vector(vector);
        let mut sums = [0i64; 3];
        for (row, sum) in sums.iter_mut().enumerate() {
            *sum = self.multiply_accumulate(row, 0, llm[row], v);
        }
        self.store_mac_and_ir(sums, command.sf, command.lm);

        self.color_matrix(command);
    }

    /// `IR = BK * 0x1000 + LCM * IR`.
    fn color_matrix(&mut self, command: Command) {
        let lcm = self.matrix(16);
        let bk = self.background();
        let ir = self.ir_vector();
        let mut sums = [0i64; 3];
        for (row, sum) in sums.iter_mut().enumerate() {
            *sum = self.multiply_accumulate(row, bk[row] * 0x1000, lcm[row], ir);
        }
        self.store_mac_and_ir(sums, command.sf, command.lm);
    }

    /// Multiplica `IR` pela cor do vértice, deixando o resultado em `MAC`.
    ///
    /// Passa por `check_mac`: é uma operação de acumulador como qualquer
    /// outra, e escrever `MAC` direto engoliria as flags de overflow.
    fn apply_vertex_color(&mut self) {
        let rgb = self.rgb();
        let ir = self.ir_vector();
        for row in 0..3 {
            let checked = self.check_mac(row + 1, (rgb[row] * ir[row]) << 4);
            self.data[MAC1 + row] = checked as i32 as u32;
        }
    }

    /// `NCS` / `NCT` — normal color: iluminação sem a cor do vértice.
    fn ncs(&mut self, vector: usize, command: Command) {
        self.light(vector, command);
        self.push_color();
    }

    /// `NCCS` / `NCCT` — normal color color: iluminação vezes a cor do vértice.
    fn nccs(&mut self, vector: usize, command: Command) {
        self.light(vector, command);
        self.apply_vertex_color();
        let macs = [
            self.data[MAC1] as i32 as i64,
            self.data[MAC1 + 1] as i32 as i64,
            self.data[MAC1 + 2] as i32 as i64,
        ];
        self.set_mac_and_ir(macs, command.sf, command.lm);
        self.push_color();
    }

    /// `NCDS` / `NCDT` — normal color depth cue: como `NCCS`, mais a
    /// interpolação para a far color.
    fn ncds(&mut self, vector: usize, command: Command) {
        self.light(vector, command);
        self.apply_vertex_color();
        self.interpolate(command);
        self.push_color();
    }

    /// `CC` — color color: parte de `IR` já pronto, sem a matriz de luz.
    fn cc(&mut self, command: Command) {
        self.color_matrix(command);
        self.apply_vertex_color();
        let macs = [
            self.data[MAC1] as i32 as i64,
            self.data[MAC1 + 1] as i32 as i64,
            self.data[MAC1 + 2] as i32 as i64,
        ];
        self.set_mac_and_ir(macs, command.sf, command.lm);
        self.push_color();
    }

    /// `CDP` — color depth cue: como `CC`, mais a interpolação.
    fn cdp(&mut self, command: Command) {
        self.color_matrix(command);
        self.apply_vertex_color();
        self.interpolate(command);
        self.push_color();
    }

    /// `GPF` — general purpose interpolation: `MAC = IR * IR0`.
    fn gpf(&mut self, command: Command) {
        let ir0 = self.data[IR0] as i16 as i64;
        let ir = self.ir_vector();
        self.set_mac_and_ir(
            [ir[0] * ir0, ir[1] * ir0, ir[2] * ir0],
            command.sf,
            command.lm,
        );
        self.push_color();
    }

    /// `GPL` — como `GPF`, mas somando o `MAC` corrente já desdeslocado.
    fn gpl(&mut self, command: Command) {
        let ir0 = self.data[IR0] as i16 as i64;
        let ir = self.ir_vector();
        let mut sums = [0i64; 3];
        for (row, sum) in sums.iter_mut().enumerate() {
            let mac = self.data[MAC1 + row] as i32 as i64;
            *sum = (mac << command.sf) + ir[row] * ir0;
        }
        self.set_mac_and_ir(sums, command.sf, command.lm);
        self.push_color();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Índices de controle usados pelos testes, com os nomes do PSX-SPX.
    const RT11RT12: usize = 0;
    const RT22RT23: usize = 2;
    const RT33: usize = 4;
    const H: usize = 26;
    const DQA: usize = 27;
    const DQB: usize = 28;
    const ZSF3: usize = 29;
    const ZSF4: usize = 30;
    const FLAG: usize = 31;

    /// `sf` ligado (bit 19) — a forma como quase todo jogo emite os comandos.
    const SF: u32 = 1 << 19;
    /// `lm` ligado (bit 10).
    const LM: u32 = 1 << 10;

    /// GTE com a matriz de rotação em identidade (1.0 = 0x1000).
    fn identity() -> Gte {
        let mut gte = Gte::new();
        gte.write_control(RT11RT12, 0x1000);
        gte.write_control(RT22RT23, 0x1000);
        gte.write_control(RT33, 0x1000);
        gte
    }

    /// Escreve `V0 = (x, y, z)`.
    fn set_v0(gte: &mut Gte, x: i16, y: i16, z: i16) {
        gte.write_data(0, (x as u16 as u32) | ((y as u16 as u32) << 16));
        gte.write_data(1, z as u16 as u32);
    }

    fn set_ir(gte: &mut Gte, ir1: i16, ir2: i16, ir3: i16) {
        gte.write_data(9, ir1 as u16 as u32);
        gte.write_data(10, ir2 as u16 as u32);
        gte.write_data(11, ir3 as u16 as u32);
    }

    fn set_sxy(gte: &mut Gte, index: usize, x: i16, y: i16) {
        gte.write_data(12 + index, (x as u16 as u32) | ((y as u16 as u32) << 16));
    }

    // ------------------------------------------------------------- divisão

    #[test]
    fn the_unr_table_matches_the_hardware_formula() {
        assert_eq!(UNR_TABLE[0], 0xFF);
        assert_eq!(UNR_TABLE[256], 0x00);
        // Um valor do meio, conferido contra (0x40000/(i+0x100)+1)/2-0x101.
        assert_eq!(UNR_TABLE[128], 84);
    }

    #[test]
    fn division_matches_exact_arithmetic_inside_its_valid_range() {
        let mut gte = Gte::new();
        // A estimativa UNR é exata sempre que H < SZ3*2, que é o caso de uso.
        for (h, sz3) in [(4u32, 3u32), (100, 60), (1000, 700), (0xFFFF, 0x8000)] {
            let expected = (h as u64 * 0x2_0000 / sz3 as u64).div_ceil(2);
            assert_eq!(
                gte.divide(h, sz3) as u64,
                expected.min(0x1FFFF),
                "H={h} SZ3={sz3}"
            );
        }
        assert_eq!(gte.read_control(FLAG) & Flag::DIVIDE_OVERFLOW.0, 0);
    }

    #[test]
    fn division_saturates_and_flags_when_the_vertex_is_too_close() {
        let mut gte = Gte::new();
        // H >= SZ3*2 significa que o vértice passou do plano de projeção.
        assert_eq!(gte.divide(0x2000, 1), 0x1FFFF);
        assert_ne!(gte.read_control(FLAG) & Flag::DIVIDE_OVERFLOW.0, 0);
    }

    #[test]
    fn dividing_by_zero_flags_instead_of_panicking() {
        let mut gte = Gte::new();
        assert_eq!(gte.divide(1, 0), 0x1FFFF);
        assert_ne!(gte.read_control(FLAG) & Flag::DIVIDE_OVERFLOW.0, 0);
    }

    // ---------------------------------------------------------------- RTPS

    #[test]
    fn rtps_projects_a_vertex_through_the_identity_matrix() {
        let mut gte = identity();
        set_v0(&mut gte, 1, 2, 3);
        gte.write_control(H, 4);

        gte.execute(SF | 0x01);

        assert_eq!(gte.read_data(25), 1, "MAC1");
        assert_eq!(gte.read_data(26), 2, "MAC2");
        assert_eq!(gte.read_data(27), 3, "MAC3");
        assert_eq!(gte.read_data(9), 1, "IR1");
        assert_eq!(gte.read_data(10), 2, "IR2");
        assert_eq!(gte.read_data(11), 3, "IR3");
        assert_eq!(gte.read_data(19), 3, "SZ3");

        // divide(4, 3) = 87381; SX2 = 87381>>16 = 1, SY2 = 174762>>16 = 2.
        assert_eq!(gte.read_data(14) & 0xFFFF, 1, "SX2");
        assert_eq!(gte.read_data(14) >> 16, 2, "SY2");
        assert_eq!(gte.read_control(FLAG), 0, "nenhuma saturação");
    }

    #[test]
    fn rtps_applies_the_screen_offset() {
        let mut gte = identity();
        set_v0(&mut gte, 1, 1, 3);
        gte.write_control(H, 4);
        gte.write_control(24, (100i32 << 16) as u32); // OFX = 100.0
        gte.write_control(25, (-50i32 << 16) as u32); // OFY = -50.0

        gte.execute(SF | 0x01);

        assert_eq!(gte.read_data(14) as i16, 101, "SX2 = 1 + OFX");
        assert_eq!((gte.read_data(14) >> 16) as i16, -49, "SY2 = 1 + OFY");
    }

    #[test]
    fn rtps_computes_depth_cueing_into_ir0() {
        let mut gte = identity();
        set_v0(&mut gte, 0, 0, 3);
        gte.write_control(H, 4);
        gte.write_control(DQA, 0x10);
        gte.write_control(DQB, 0x1000);

        gte.execute(SF | 0x01);

        // MAC0 = 87381*0x10 + 0x1000; IR0 = MAC0>>12.
        let mac0 = 87381i64 * 0x10 + 0x1000;
        assert_eq!(gte.read_data(24), mac0 as u32, "MAC0");
        assert_eq!(gte.read_data(8), ((mac0 >> 12) as u32).min(0x1000), "IR0");
    }

    #[test]
    fn ir0_saturates_at_1000h() {
        let mut gte = identity();
        set_v0(&mut gte, 0, 0, 3);
        gte.write_control(H, 4);
        // DQA grande, mas sem estourar MAC0: o que satura é o IR0.
        gte.write_control(DQA, 0x1000);

        gte.execute(SF | 0x01);

        assert_eq!(gte.read_data(8), 0x1000, "IR0 satura no teto");
        assert_ne!(gte.read_control(FLAG) & Flag::IR0_SATURATED.0, 0);
        // Quirk: IR0 não liga o bit 31.
        assert_eq!(gte.read_control(FLAG) & 0x8000_0000, 0);
    }

    #[test]
    fn rtpt_projects_three_vertices_into_the_screen_fifo() {
        let mut gte = identity();
        set_v0(&mut gte, 1, 1, 3);
        gte.write_data(2, 2 | (2 << 16)); // V1 = (2,2,·)
        gte.write_data(3, 3);
        gte.write_data(4, 4 | (4 << 16)); // V2 = (4,4,·)
        gte.write_data(5, 3);
        gte.write_control(H, 4);

        gte.execute(SF | 0x30);

        assert_eq!(gte.read_data(12) as i16, 1, "SXY0 veio de V0");
        assert_eq!(gte.read_data(13) as i16, 2, "SXY1 veio de V1");
        assert_eq!(gte.read_data(14) as i16, 5, "SXY2 veio de V2");
        // IR1..3 refletem o último vértice.
        assert_eq!(gte.read_data(9), 4);
    }

    #[test]
    fn sx2_saturates_and_flags_outside_the_screen_range() {
        let mut gte = identity();
        set_v0(&mut gte, 0x400, 0, 1);
        gte.write_control(H, 1);

        gte.execute(SF | 0x01);

        assert_eq!(gte.read_data(14) as i16, 0x3FF, "SX2 satura em +1023");
        assert_ne!(gte.read_control(FLAG) & Flag::SX2_SATURATED.0, 0);
        assert_ne!(gte.read_control(FLAG) & 0x8000_0000, 0, "SX2 liga o bit 31");
    }

    // --------------------------------------------------------------- NCLIP

    #[test]
    fn nclip_gives_the_signed_area_of_the_screen_triangle() {
        let mut gte = Gte::new();
        set_sxy(&mut gte, 0, 0, 0);
        set_sxy(&mut gte, 1, 10, 0);
        set_sxy(&mut gte, 2, 0, 10);

        gte.execute(0x06);

        assert_eq!(gte.read_data(24) as i32, 100, "área positiva");
    }

    #[test]
    fn nclip_flips_sign_with_the_winding_order() {
        let mut gte = Gte::new();
        set_sxy(&mut gte, 0, 0, 0);
        set_sxy(&mut gte, 1, 0, 10);
        set_sxy(&mut gte, 2, 10, 0);

        gte.execute(0x06);

        assert_eq!(
            gte.read_data(24) as i32,
            -100,
            "ordem invertida, área negativa"
        );
    }

    // -------------------------------------------------------- AVSZ3 / AVSZ4

    #[test]
    fn avsz3_averages_the_last_three_z_values() {
        let mut gte = Gte::new();
        gte.write_data(17, 300); // SZ1
        gte.write_data(18, 300); // SZ2
        gte.write_data(19, 300); // SZ3
        gte.write_control(ZSF3, 0x555); // ~1/3 em 12 bits

        gte.execute(0x2D);

        assert_eq!(gte.read_data(24) as i32, 0x555 * 900, "MAC0");
        assert_eq!(gte.read_data(7), 299, "OTZ = MAC0>>12");
    }

    #[test]
    fn avsz4_averages_all_four() {
        let mut gte = Gte::new();
        for index in 16..20 {
            gte.write_data(index, 400);
        }
        gte.write_control(ZSF4, 0x400); // 1/4

        gte.execute(0x2E);

        assert_eq!(gte.read_data(7), 400, "OTZ");
    }

    #[test]
    fn otz_saturates_and_flags() {
        let mut gte = Gte::new();
        for index in 16..20 {
            gte.write_data(index, 0xFFFF);
        }
        // ZSF4 escolhido para estourar OTZ sem estourar MAC0 antes.
        gte.write_control(ZSF4, 0x1000);

        gte.execute(0x2E);

        assert_eq!(gte.read_data(7), 0xFFFF);
        assert_ne!(gte.read_control(FLAG) & Flag::SZ3_OTZ_SATURATED.0, 0);
    }

    // ----------------------------------------------------------- SQR / OP

    #[test]
    fn sqr_squares_the_ir_vector() {
        let mut gte = Gte::new();
        set_ir(&mut gte, 2, 3, 4);

        gte.execute(0x28); // sf = 0

        assert_eq!(gte.read_data(25), 4);
        assert_eq!(gte.read_data(26), 9);
        assert_eq!(gte.read_data(27), 16);
        assert_eq!(gte.read_data(9), 4, "IR1 recebe MAC1");
    }

    #[test]
    fn op_is_the_cross_product_with_the_matrix_diagonal() {
        let mut gte = identity();
        set_ir(&mut gte, 1, 2, 3);

        gte.execute(SF | 0x0C);

        // (IR3*D2 - IR2*D3, IR1*D3 - IR3*D1, IR2*D1 - IR1*D2) >> 12
        assert_eq!(gte.read_data(25) as i32, 1, "3-2");
        assert_eq!(gte.read_data(26) as i32, -2, "1-3");
        assert_eq!(gte.read_data(27) as i32, 1, "2-1");
    }

    // --------------------------------------------------------------- flags

    #[test]
    fn the_lm_bit_clamps_ir_at_zero_instead_of_minus_8000() {
        // D1 = 1.0, D2 = D3 = 0 deixa MAC2 = -IR3*D1, negativo.
        let build = || {
            let mut gte = Gte::new();
            gte.write_control(RT11RT12, 0x1000);
            set_ir(&mut gte, 0, 0, 1);
            gte
        };

        let mut without = build();
        without.execute(SF | 0x0C);
        assert_eq!(
            without.read_data(10) as i16,
            -1,
            "sem lm, IR2 fica negativo"
        );

        let mut with = build();
        with.execute(SF | LM | 0x0C);
        assert_eq!(with.read_data(10), 0, "com lm, IR2 satura em 0");
        assert_ne!(with.read_control(FLAG) & Flag::IR2_SATURATED.0, 0);
    }

    #[test]
    fn mac_overflow_is_detected_at_44_bits() {
        let mut gte = Gte::new();
        // Matriz e vetor no máximo, com sf = 0: o acumulado passa de 44 bits.
        for control in [0, 1, 2, 3] {
            gte.write_control(control, 0x7FFF | (0x7FFF << 16));
        }
        gte.write_control(4, 0x7FFF);
        gte.write_control(5, 0x7FFF_FFFF); // TRX enorme
        set_v0(&mut gte, 0x7FFF, 0x7FFF, 0x7FFF);

        gte.execute(0x01); // sf = 0

        assert_ne!(
            gte.read_control(FLAG) & Flag::MAC1_OVERFLOW_POSITIVE.0,
            0,
            "MAC1 estourou"
        );
        assert_ne!(gte.read_control(FLAG) & 0x8000_0000, 0, "liga o bit 31");
    }

    #[test]
    fn flag_is_cleared_at_the_start_of_every_command() {
        let mut gte = Gte::new();
        gte.write_control(FLAG, Flag::MAC1_OVERFLOW_POSITIVE.0);
        assert_ne!(gte.read_control(FLAG), 0);

        set_sxy(&mut gte, 0, 0, 0);
        set_sxy(&mut gte, 1, 1, 0);
        set_sxy(&mut gte, 2, 0, 1);
        gte.execute(0x06); // NCLIP, sem saturação

        assert_eq!(gte.read_control(FLAG), 0);
    }

    // --------------------------------------------------------------- cores

    #[test]
    fn colours_saturate_at_255_and_flag_each_channel() {
        let mut gte = Gte::new();
        set_ir(&mut gte, 0x7FFF, 0x7FFF, 0x7FFF);
        gte.write_data(8, 0x1000); // IR0 = 1.0

        // Com sf=12, MAC = 0x7FFF (IR não satura) e só a cor estoura em >>4.
        gte.execute(SF | 0x3D);

        let colour = gte.read_data(22);
        assert_eq!(colour & 0xFF, 0xFF, "R saturado");
        assert_eq!((colour >> 8) & 0xFF, 0xFF, "G saturado");
        assert_eq!((colour >> 16) & 0xFF, 0xFF, "B saturado");
        assert_ne!(gte.read_control(FLAG) & Flag::COLOR_R_SATURATED.0, 0);
        // Quirk: saturação de cor não liga o bit 31.
        assert_eq!(gte.read_control(FLAG) & 0x8000_0000, 0);
    }

    #[test]
    fn the_colour_fifo_shifts_on_every_push() {
        let mut gte = Gte::new();
        gte.write_data(20, 0x11);
        gte.write_data(21, 0x22);
        gte.write_data(22, 0x33);
        set_ir(&mut gte, 0, 0, 0);
        gte.write_data(8, 0);

        gte.execute(0x3D); // GPF empurra uma cor nova

        assert_eq!(gte.read_data(20), 0x22, "RGB0 recebeu RGB1");
        assert_eq!(gte.read_data(21), 0x33, "RGB1 recebeu RGB2");
        assert_eq!(gte.read_data(22) & 0xFF_FFFF, 0, "RGB2 é o novo");
    }

    #[test]
    fn the_colour_code_byte_survives_the_fifo() {
        let mut gte = Gte::new();
        gte.write_data(6, 0x2A00_0000); // RGBC com CODE = 0x2A
        set_ir(&mut gte, 0, 0, 0);
        gte.write_data(8, 0);

        gte.execute(0x3D);

        assert_eq!(gte.read_data(22) >> 24, 0x2A, "CODE é copiado do RGBC");
    }

    // -------------------------------------------------------------- MVMVA

    #[test]
    fn mvmva_multiplies_the_chosen_matrix_by_the_chosen_vector() {
        let mut gte = identity();
        set_v0(&mut gte, 3, 4, 5);
        // mx=0 (rotação), v=0 (V0), cv=3 (sem translação).
        gte.execute(SF | (3 << 13) | 0x12);

        assert_eq!(gte.read_data(25) as i32, 3);
        assert_eq!(gte.read_data(26) as i32, 4);
        assert_eq!(gte.read_data(27) as i32, 5);
    }

    #[test]
    fn mvmva_adds_the_translation_vector() {
        let mut gte = identity();
        set_v0(&mut gte, 1, 1, 1);
        gte.write_control(5, 10); // TRX
        gte.write_control(6, 20); // TRY
        gte.write_control(7, 30); // TRZ

        gte.execute(SF | 0x12); // cv = 0 (TR)

        assert_eq!(gte.read_data(25) as i32, 11, "1 + TRX");
        assert_eq!(gte.read_data(26) as i32, 21);
        assert_eq!(gte.read_data(27) as i32, 31);
    }

    #[test]
    fn mvmva_can_take_ir_as_the_input_vector() {
        let mut gte = identity();
        set_ir(&mut gte, 7, 8, 9);
        // v = 3 (IR), cv = 3 (nenhuma).
        gte.execute(SF | (3 << 15) | (3 << 13) | 0x12);

        assert_eq!(gte.read_data(25) as i32, 7);
        assert_eq!(gte.read_data(26) as i32, 8);
        assert_eq!(gte.read_data(27) as i32, 9);
    }

    #[test]
    fn mvmva_with_far_colour_reproduces_the_hardware_bug() {
        let mut gte = identity();
        set_v0(&mut gte, 1, 1, 1);
        gte.write_control(21, 0x100); // RFC
        gte.write_control(22, 0x100); // GFC
        gte.write_control(23, 0x100); // BFC

        // cv = 2 (far color) — bugado: o primeiro produto some junto com FC.
        gte.execute(SF | (2 << 13) | 0x12);

        // O que sobrevive é só `m[linha][1]*v[1] + m[linha][2]*v[2]`: tanto o
        // FC quanto o primeiro produto da linha são jogados fora.
        //
        // Na identidade, a linha 0 tem esses dois termos zerados, então MAC1
        // fica em 0 — sem o bug seria 0x100 (FC) + 1 (a diagonal). As linhas 1
        // e 2 mantêm a diagonal no termo que sobrevive, e dão 1.
        assert_eq!(gte.read_data(25) as i32, 0, "linha 0 perde tudo");
        assert_eq!(gte.read_data(26) as i32, 1, "sobra a diagonal, sem o FC");
        assert_eq!(gte.read_data(27) as i32, 1, "idem");
    }

    // ---------------------------------------------------------- iluminação

    #[test]
    fn ncs_runs_the_light_and_colour_matrices() {
        let mut gte = Gte::new();
        // Matrizes de luz e de cor em identidade.
        for control in [8, 10, 12, 16, 18, 20] {
            gte.write_control(control, 0x1000);
        }
        set_v0(&mut gte, 0x10, 0x20, 0x30);

        gte.execute(SF | 0x1E);

        // LLM*V = V; depois LCM*IR = IR, com BK = 0.
        assert_eq!(gte.read_data(9) as i32, 0x10, "IR1");
        assert_eq!(gte.read_data(10) as i32, 0x20, "IR2");
        assert_eq!(gte.read_data(11) as i32, 0x30, "IR3");
        // A cor sai de MAC>>4.
        assert_eq!(gte.read_data(22) & 0xFF, 0x01);
    }

    #[test]
    fn nct_processes_all_three_normals() {
        let mut gte = Gte::new();
        for control in [8, 10, 12, 16, 18, 20] {
            gte.write_control(control, 0x1000);
        }
        set_v0(&mut gte, 0x10, 0, 0);
        gte.write_data(2, 0x20);
        gte.write_data(4, 0x30);

        gte.execute(SF | 0x20);

        // Três pushes na FIFO de cor, na ordem V0, V1, V2.
        assert_eq!(gte.read_data(20) & 0xFF, 0x01, "0x10>>4");
        assert_eq!(gte.read_data(21) & 0xFF, 0x02, "0x20>>4");
        assert_eq!(gte.read_data(22) & 0xFF, 0x03, "0x30>>4");
    }

    #[test]
    fn intpl_with_ir0_at_zero_keeps_the_original_colour() {
        let mut gte = Gte::new();
        set_ir(&mut gte, 0x100, 0x100, 0x100);
        gte.write_data(8, 0); // IR0 = 0
        for control in [21, 22, 23] {
            gte.write_control(control, 0x7F);
        }

        gte.execute(SF | 0x11);

        // (0x100<<12)>>12>>4 = 0x10.
        assert_eq!(gte.read_data(22) & 0xFF, 0x10);
    }

    #[test]
    fn intpl_with_ir0_at_one_reaches_the_far_colour() {
        let mut gte = Gte::new();
        set_ir(&mut gte, 0x100, 0x100, 0x100);
        gte.write_data(8, 0x1000); // IR0 = 1.0
        for control in [21, 22, 23] {
            gte.write_control(control, 0x200);
        }

        gte.execute(SF | 0x11);

        // MAC = MAC + (FC - MAC)*1.0 = FC; cor = 0x200>>4.
        assert_eq!(gte.read_data(22) & 0xFF, 0x20);
    }

    #[test]
    fn dpcs_starts_from_the_vertex_colour() {
        let mut gte = Gte::new();
        gte.write_data(6, 0x30 | (0x40 << 8) | (0x50 << 16));
        gte.write_data(8, 0); // IR0 = 0

        gte.execute(SF | 0x10);

        // (canal<<16)>>12>>4 = canal.
        assert_eq!(gte.read_data(22) & 0xFF, 0x30);
        assert_eq!((gte.read_data(22) >> 8) & 0xFF, 0x40);
        assert_eq!((gte.read_data(22) >> 16) & 0xFF, 0x50);
    }

    #[test]
    fn gpl_accumulates_onto_the_current_mac() {
        let mut gte = Gte::new();
        set_ir(&mut gte, 1, 1, 1);
        gte.write_data(8, 0x1000); // IR0 = 1.0
        gte.write_data(25, 0x10); // MAC1 corrente
        gte.write_data(26, 0x20);
        gte.write_data(27, 0x30);

        gte.execute(0x3E); // sf = 0

        // sf = 0: MAC = MAC + IR*IR0.
        assert_eq!(gte.read_data(25) as i32, 0x10 + 0x1000);
        assert_eq!(gte.read_data(26) as i32, 0x20 + 0x1000);
    }

    #[test]
    fn every_documented_opcode_is_implemented() {
        // Se algum opcode da tabela de ciclos cair no contador, a tabela e o
        // dispatch saíram de sincronia.
        let opcodes = [
            0x01, 0x06, 0x0C, 0x10, 0x11, 0x12, 0x13, 0x14, 0x16, 0x1B, 0x1C, 0x1E, 0x20, 0x28,
            0x29, 0x2A, 0x2D, 0x2E, 0x30, 0x3D, 0x3E, 0x3F,
        ];
        let mut gte = Gte::new();
        for opcode in opcodes {
            gte.execute(SF | opcode);
        }
        assert_eq!(
            gte.unimplemented_commands(),
            0,
            "último não implementado: {:#04X}",
            gte.last_unimplemented_command()
        );
    }
}
