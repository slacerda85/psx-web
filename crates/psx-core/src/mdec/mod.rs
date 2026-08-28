//! MDEC — o decodificador de macroblocos.
//!
//! Referência: PSX-SPX — "Macroblock Decoder (MDEC)".
//!
//! É o que torna FMV possível no console: o jogo lê do CD blocos de
//! coeficientes DCT quantizados e o MDEC devolve pixels. Sem ele, qualquer
//! jogo que abra com vídeo trava esperando dados que nunca chegam.
//!
//! Duas portas:
//!
//! | Endereço      | Escrita           | Leitura         |
//! |---------------|-------------------|-----------------|
//! | `0x1F80_1820` | comando/parâmetro | dados de saída  |
//! | `0x1F80_1824` | controle/reset    | status          |

use std::collections::VecDeque;

/// Coeficientes por bloco (8×8).
const BLOCK: usize = 64;

/// Ordem de varredura em ziguezague usada na descompressão RLE.
#[rustfmt::skip]
const ZIGZAG: [usize; BLOCK] = [
     0,  1,  8, 16,  9,  2,  3, 10,
    17, 24, 32, 25, 18, 11,  4,  5,
    12, 19, 26, 33, 40, 48, 41, 34,
    27, 20, 13,  6,  7, 14, 21, 28,
    35, 42, 49, 56, 57, 50, 43, 36,
    29, 22, 15, 23, 30, 37, 44, 51,
    58, 59, 52, 45, 38, 31, 39, 46,
    53, 60, 61, 54, 47, 55, 62, 63,
];

/// Profundidade de cor da saída (`status`, bits 25..26).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Depth {
    Bpp4,
    Bpp8,
    Bpp24,
    Bpp15,
}

impl Depth {
    const fn from_bits(bits: u32) -> Self {
        match bits {
            0 => Depth::Bpp4,
            1 => Depth::Bpp8,
            2 => Depth::Bpp24,
            _ => Depth::Bpp15,
        }
    }

    /// Monocromático usa um bloco de 8×8; colorido usa seis, formando 16×16.
    const fn is_monochrome(self) -> bool {
        matches!(self, Depth::Bpp4 | Depth::Bpp8)
    }
}

/// O que o MDEC espera receber na próxima palavra.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expecting {
    Command,
    /// Coeficientes de macrobloco; faltam `remaining` palavras.
    Data,
    /// Tabelas de quantização: 16 palavras (mono) ou 32 (cor).
    QuantTable {
        color: bool,
    },
    /// Tabela de escala do IDCT: 32 palavras.
    ScaleTable,
}

/// O decodificador.
#[derive(Debug, Clone)]
pub struct Mdec {
    expecting: Expecting,
    /// Palavras ainda esperadas do comando corrente.
    remaining: u32,
    /// Algum comando já passou por aqui desde o reset.
    ///
    /// O campo de palavras restantes do `MDEC_STATUS` só ganha sentido depois
    /// do primeiro comando: num MDEC recém-ligado o console reporta 0x0000
    /// (`cpu/io-access-bitwidth`), e depois de um comando terminar reporta
    /// 0xFFFF, que é "nenhuma" menos um (`mdec/step-by-step-log`).
    command_seen: bool,
    /// Palavras recebidas do comando corrente.
    received: Vec<u32>,

    quant_luma: [u8; BLOCK],
    quant_chroma: [u8; BLOCK],
    scale: [i16; BLOCK],

    /// Coeficientes crus do macrobloco em decodificação.
    input: VecDeque<u16>,
    /// Pixels prontos, em palavras, na ordem em que o CPU/DMA os lê.
    output: VecDeque<u32>,

    depth: Depth,
    /// Bit 24 do comando — saída com sinal.
    signed: bool,
    /// Bit 25 do comando — valor do bit 15 na saída de 15 bpp.
    mask_bit: bool,

    /// Bit 30 do controle — DMA de entrada habilitado.
    dma_in: bool,
    /// Bit 29 do controle — DMA de saída habilitado.
    dma_out: bool,

    /// Comandos que caíram fora da tabela, para diagnóstico.
    unimplemented: u64,
}

impl Mdec {
    pub fn new() -> Self {
        Self {
            expecting: Expecting::Command,
            remaining: 0,
            command_seen: false,
            received: Vec::new(),
            quant_luma: [0; BLOCK],
            quant_chroma: [0; BLOCK],
            scale: [0; BLOCK],
            input: VecDeque::new(),
            output: VecDeque::new(),
            depth: Depth::Bpp4,
            signed: false,
            mask_bit: false,
            dma_in: false,
            dma_out: false,
            unimplemented: 0,
        }
    }

    pub const fn unimplemented_commands(&self) -> u64 {
        self.unimplemented
    }

    pub fn reset(&mut self) {
        let quant_luma = self.quant_luma;
        let quant_chroma = self.quant_chroma;
        let scale = self.scale;
        *self = Self::new();
        // O reset do MDEC não apaga as tabelas carregadas.
        self.quant_luma = quant_luma;
        self.quant_chroma = quant_chroma;
        self.scale = scale;
    }

    // ----------------------------------------------------------- registradores

    /// `0x1F80_1824` — status.
    pub fn status(&self) -> u32 {
        let mut status = 0u32;

        // Bits 0..15: palavras restantes **menos um**, e o menos um vale também
        // quando não há nenhuma: o console reporta 0xFFFF em repouso, como o
        // `mdec/step-by-step-log` mostra na última leitura (0x8604FFFF).
        // Reportar zero ali faz o software concluir que ainda falta uma palavra.
        status |= if self.command_seen {
            self.remaining.wrapping_sub(1) & 0xFFFF
        } else {
            0
        };

        // Bits 16..18: bloco corrente. Sem modelar o pipeline interno, o valor
        // só precisa ser estável.
        status |= 4 << 16;

        // Bit 23: valor do bit 15 na saída de 15 bpp.
        status |= (self.mask_bit as u32) << 23;
        // Bit 24: saída com sinal.
        status |= (self.signed as u32) << 24;
        // Bits 25..26: profundidade.
        status |= (self.depth as u32) << 25;
        // Bit 27: pedido de DMA de saída.
        status |= ((self.dma_out && !self.output.is_empty()) as u32) << 27;
        // Bit 28: pedido de DMA de entrada.
        status |= ((self.dma_in && self.remaining > 0) as u32) << 28;
        // Bit 29: comando em andamento.
        status |= ((self.expecting != Expecting::Command) as u32) << 29;
        // Bit 30: FIFO de entrada cheia — nunca, a decodificação é imediata.
        // Bit 31: FIFO de saída vazia.
        status |= (self.output.is_empty() as u32) << 31;

        status
    }

    /// Palavras decodificadas à espera de serem lidas.
    pub fn pending_output(&self) -> usize {
        self.output.len()
    }

    /// `0x1F80_1820` — leitura da saída decodificada.
    pub fn read_data(&mut self) -> u32 {
        self.output.pop_front().unwrap_or(0)
    }

    /// `0x1F80_1824` — escrita de controle.
    pub fn write_control(&mut self, value: u32) {
        if value & (1 << 31) != 0 {
            self.reset();
            return;
        }
        self.dma_in = value & (1 << 30) != 0;
        self.dma_out = value & (1 << 29) != 0;
    }

    /// `0x1F80_1820` — escrita de comando ou parâmetro.
    pub fn write_command(&mut self, value: u32) {
        match self.expecting {
            Expecting::Command => self.start_command(value),
            Expecting::Data => {
                // Cada palavra traz dois coeficientes de 16 bits.
                self.input.push_back(value as u16);
                self.input.push_back((value >> 16) as u16);
                self.consume_word();
                if self.remaining == 0 {
                    self.decode_all();
                    self.expecting = Expecting::Command;
                }
            }
            Expecting::QuantTable { color } => {
                self.received.push(value);
                self.consume_word();
                if self.remaining == 0 {
                    self.load_quant_tables(color);
                    self.expecting = Expecting::Command;
                }
            }
            Expecting::ScaleTable => {
                self.received.push(value);
                self.consume_word();
                if self.remaining == 0 {
                    self.load_scale_table();
                    self.expecting = Expecting::Command;
                }
            }
        }
    }

    fn consume_word(&mut self) {
        self.remaining = self.remaining.saturating_sub(1);
    }

    fn start_command(&mut self, value: u32) {
        // Os bits de formato vêm no próprio comando, qualquer que seja ele.
        self.depth = Depth::from_bits((value >> 27) & 3);
        self.signed = value & (1 << 26) != 0;
        self.mask_bit = value & (1 << 25) != 0;
        self.received.clear();
        self.command_seen = true;

        match value >> 29 {
            // Decodificar macrobloco(s): o parâmetro é o tamanho em palavras.
            1 => {
                self.remaining = value & 0xFFFF;
                self.input.clear();
                self.output.clear();
                self.expecting = if self.remaining == 0 {
                    Expecting::Command
                } else {
                    Expecting::Data
                };
            }
            // Tabelas de quantização: 16 palavras (mono) ou 32 (cor).
            2 => {
                let color = value & 1 != 0;
                self.remaining = if color { 32 } else { 16 };
                self.expecting = Expecting::QuantTable { color };
            }
            // Tabela de escala do IDCT: 32 palavras, 64 meias-palavras.
            3 => {
                self.remaining = 32;
                self.expecting = Expecting::ScaleTable;
            }
            _ => {
                // Comandos 0 e 4..7 não fazem nada além de ajustar o formato.
                self.unimplemented += 1;
                self.remaining = 0;
                self.expecting = Expecting::Command;
            }
        }
    }

    fn load_quant_tables(&mut self, color: bool) {
        for (index, word) in self.received.iter().enumerate() {
            for byte in 0..4 {
                let value = (word >> (byte * 8)) as u8;
                let position = index * 4 + byte;
                if position < BLOCK {
                    self.quant_luma[position] = value;
                } else if position < BLOCK * 2 {
                    self.quant_chroma[position - BLOCK] = value;
                }
            }
        }
        if !color {
            // Sem tabela de croma, o hardware mantém a anterior.
            self.quant_chroma = self.quant_luma;
        }
    }

    fn load_scale_table(&mut self) {
        for (index, word) in self.received.iter().enumerate() {
            self.scale[index * 2] = *word as i16;
            self.scale[index * 2 + 1] = (*word >> 16) as i16;
        }
    }

    // -------------------------------------------------------------- decodificação

    /// Decodifica todos os macroblocos que couberem nos coeficientes recebidos.
    fn decode_all(&mut self) {
        if self.depth.is_monochrome() {
            while let Some(block) = self.decode_block(true) {
                self.emit_monochrome(&block);
            }
        } else {
            // Um macrobloco colorido são seis blocos: Cr, Cb e quatro de luma.
            while let Some(cr) = self.decode_block(false) {
                let Some(cb) = self.decode_block(false) else {
                    break;
                };
                let mut luma = [[0i16; BLOCK]; 4];
                let mut complete = true;
                for quadrant in luma.iter_mut() {
                    match self.decode_block(true) {
                        Some(block) => *quadrant = block,
                        None => {
                            complete = false;
                            break;
                        }
                    }
                }
                if !complete {
                    break;
                }
                self.emit_color(&cr, &cb, &luma);
            }
        }
    }

    /// Descomprime um bloco RLE, dequantiza e aplica o IDCT.
    ///
    /// Devolve `None` quando os coeficientes acabaram.
    fn decode_block(&mut self, luma: bool) -> Option<[i16; BLOCK]> {
        // 0xFE00 é o preenchimento entre blocos.
        while self.input.front() == Some(&0xFE00) {
            self.input.pop_front();
        }
        let first = self.input.pop_front()?;

        let quant = if luma {
            self.quant_luma
        } else {
            self.quant_chroma
        };
        let scale_factor = ((first >> 10) & 0x3F) as i32;
        let mut coefficients = [0i16; BLOCK];

        // O primeiro coeficiente é o DC: multiplicado só por `qt[0]`, sem o
        // fator de escala nem o arredondamento dos demais.
        let mut current = first;
        let mut value = signed10(first) * quant[0] as i32;
        let mut index = 0usize;

        loop {
            // Com fator de escala zero o hardware ignora a tabela e usa o
            // coeficiente cru dobrado — inclusive nos termos AC.
            if scale_factor == 0 {
                value = signed10(current) * 2;
            }
            let clamped = value.clamp(-0x400, 0x3FF) as i16;
            // E escreve na ordem natural, não na do ziguezague.
            let position = if scale_factor > 0 {
                ZIGZAG[index]
            } else {
                index
            };
            coefficients[position] = clamped;

            let Some(next) = self.input.pop_front() else {
                break;
            };
            current = next;
            // Os seis bits altos dizem quantos zeros pular.
            index += (((next >> 10) & 0x3F) as usize) + 1;
            if index > 63 {
                break;
            }
            value = (signed10(next) * quant[index] as i32 * scale_factor + 4) / 8;
        }

        self.idct(&mut coefficients);
        Some(coefficients)
    }

    /// IDCT bidimensional pela tabela de escala carregada.
    fn idct(&self, block: &mut [i16; BLOCK]) {
        let mut temporary = [0i16; BLOCK];
        for pass in 0..2 {
            let (source, destination): (&[i16; BLOCK], &mut [i16; BLOCK]) = if pass == 0 {
                (&*block, &mut temporary)
            } else {
                (&temporary, block)
            };
            for x in 0..8 {
                for y in 0..8 {
                    let mut sum = 0i64;
                    for z in 0..8 {
                        sum += source[y + z * 8] as i64 * (self.scale[x + z * 8] as i64 >> 3);
                    }
                    destination[x + y * 8] = ((sum + 0x0FFF) >> 13) as i16;
                }
            }
        }
    }

    // ------------------------------------------------------------------- saída

    fn emit_monochrome(&mut self, block: &[i16; BLOCK]) {
        let mut pixels = [0u8; BLOCK];
        for (destination, source) in pixels.iter_mut().zip(block.iter()) {
            let value = source.clamp(&-128, &127);
            *destination = if self.signed {
                *value as u8
            } else {
                (*value as u8) ^ 0x80
            };
        }

        match self.depth {
            Depth::Bpp4 => {
                for chunk in pixels.chunks(8) {
                    let mut word = 0u32;
                    for (index, pixel) in chunk.iter().enumerate() {
                        word |= ((pixel >> 4) as u32) << (index * 4);
                    }
                    self.output.push_back(word);
                }
            }
            _ => {
                for chunk in pixels.chunks(4) {
                    let mut word = 0u32;
                    for (index, pixel) in chunk.iter().enumerate() {
                        word |= (*pixel as u32) << (index * 8);
                    }
                    self.output.push_back(word);
                }
            }
        }
    }

    /// Converte YUV para RGB e empurra o macrobloco de 16×16.
    fn emit_color(&mut self, cr: &[i16; BLOCK], cb: &[i16; BLOCK], luma: &[[i16; BLOCK]; 4]) {
        let mut macroblock = [[0u8; 3]; 16 * 16];

        for (quadrant, block) in luma.iter().enumerate() {
            // Os quatro blocos de luma cobrem os quadrantes do macrobloco.
            let offset_x = (quadrant & 1) * 8;
            let offset_y = (quadrant >> 1) * 8;

            for y in 0..8 {
                for x in 0..8 {
                    // Croma tem metade da resolução: 8×8 para os 16×16 de luma.
                    let chroma = ((x + offset_x) / 2) + ((y + offset_y) / 2) * 8;
                    let r_component = cr[chroma] as i32;
                    let b_component = cb[chroma] as i32;

                    // Coeficientes do hardware em ponto fixo de 1/1024.
                    let red = (1435 * r_component) >> 10;
                    let green = (-352 * b_component - 731 * r_component) >> 10;
                    let blue = (1814 * b_component) >> 10;

                    let y_component = block[x + y * 8] as i32;
                    let pixel = [
                        (y_component + red).clamp(-128, 127),
                        (y_component + green).clamp(-128, 127),
                        (y_component + blue).clamp(-128, 127),
                    ];

                    let position = (x + offset_x) + (y + offset_y) * 16;
                    for (channel, value) in pixel.iter().enumerate() {
                        macroblock[position][channel] = if self.signed {
                            *value as u8
                        } else {
                            (*value as u8) ^ 0x80
                        };
                    }
                }
            }
        }

        match self.depth {
            Depth::Bpp15 => {
                let high = (self.mask_bit as u16) << 15;
                for pair in macroblock.chunks(2) {
                    let first = pack_bgr555(pair[0]) | high;
                    let second = pack_bgr555(pair[1]) | high;
                    self.output
                        .push_back(first as u32 | ((second as u32) << 16));
                }
            }
            _ => {
                // 24 bpp: três bytes por pixel, empacotados sem alinhamento.
                let mut bytes = Vec::with_capacity(macroblock.len() * 3);
                for pixel in &macroblock {
                    bytes.extend_from_slice(pixel);
                }
                for chunk in bytes.chunks(4) {
                    let mut word = 0u32;
                    for (index, byte) in chunk.iter().enumerate() {
                        word |= (*byte as u32) << (index * 8);
                    }
                    self.output.push_back(word);
                }
            }
        }
    }

    // --------------------------------------------------------------------- DMA

    /// Canal 0 — palavras de coeficientes vindas da RAM.
    pub fn dma_write(&mut self, word: u32) {
        self.write_command(word);
    }

    /// Canal 1 — pixels decodificados indo para a RAM.
    pub fn dma_read(&mut self) -> u32 {
        self.read_data()
    }
}

/// Estende o campo de 10 bits com sinal de um coeficiente RLE.
fn signed10(value: u16) -> i32 {
    let raw = (value & 0x3FF) as i32;
    if raw & 0x200 != 0 {
        raw - 0x400
    } else {
        raw
    }
}

fn pack_bgr555(pixel: [u8; 3]) -> u16 {
    let r = (pixel[0] >> 3) as u16;
    let g = (pixel[1] >> 3) as u16;
    let b = (pixel[2] >> 3) as u16;
    r | (g << 5) | (b << 10)
}

impl Default for Mdec {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Matriz do IDCT como o BIOS a carrega: `cos((2x+1)*y*pi/16) * 2^14`,
    /// com a primeira linha em `sqrt(1/2)`.
    fn standard_scale_table() -> [i16; BLOCK] {
        let mut table = [0i16; BLOCK];
        for y in 0..8 {
            for x in 0..8 {
                let factor = if y == 0 { (0.5f64).sqrt() } else { 1.0 };
                let value = factor
                    * ((2 * x + 1) as f64 * y as f64 * std::f64::consts::PI / 16.0).cos()
                    * 0.5
                    * 32768.0;
                table[x + y * 8] = value.round() as i16;
            }
        }
        table
    }

    #[test]
    fn a_dc_only_block_comes_out_flat() {
        let mut mdec = Mdec::new();
        mdec.scale = standard_scale_table();

        let mut block = [0i16; BLOCK];
        block[0] = 64;
        mdec.idct(&mut block);

        // Sem componentes AC, todo pixel do bloco tem que sair igual.
        let first = block[0];
        assert!(
            block.iter().all(|&value| value == first),
            "bloco não ficou chapado: {:?}",
            &block[..16]
        );
        assert!(
            (-128..=127).contains(&(first as i32)),
            "valor fora da faixa de saída: {first}"
        );
    }

    #[test]
    fn the_idct_keeps_a_flat_block_inside_the_output_range() {
        let mut mdec = Mdec::new();
        mdec.scale = standard_scale_table();

        // DC no máximo que a descompressão RLE deixa passar.
        let mut block = [0i16; BLOCK];
        block[0] = 0x3FF;
        mdec.idct(&mut block);

        assert!(
            block
                .iter()
                .all(|&value| (-256..=255).contains(&(value as i32))),
            "IDCT estourou: {:?}",
            &block[..8]
        );
    }
}
