//! Decodificação ADPCM, compartilhada pela SPU e pelo CD-ROM.
//!
//! Referência: PSX-SPX — "Sample Data (SPU-ADPCM)" e "SPU-ADPCM vs XA-ADPCM".
//!
//! O algoritmo é o mesmo nos dois formatos: cada amostra de 4 bits é
//! deslocada e somada a uma previsão feita a partir das duas amostras
//! anteriores. O que muda é o empacotamento — a SPU usa blocos de 16 bytes
//! com 28 amostras, o XA agrupa oito blocos que se intercalam nibble a nibble
//! dentro das mesmas palavras.

/// Coeficiente da amostra anterior, em 64 avos.
const FILTER_OLD: [i32; 5] = [0, 60, 115, 98, 122];
/// Coeficiente da penúltima amostra, em 64 avos.
const FILTER_OLDER: [i32; 5] = [0, 0, -52, -55, -60];

/// As duas amostras anteriores de um fluxo ADPCM.
///
/// Cada canal tem a sua: num setor XA estéreo, blocos pares e ímpares são
/// canais diferentes e não podem compartilhar a previsão.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct History {
    old: i32,
    older: i32,
}

impl History {
    pub const fn new_const() -> Self {
        Self { old: 0, older: 0 }
    }

    /// Decodifica uma amostra e a incorpora ao histórico.
    ///
    /// `shift` e `filter` vêm do cabeçalho do bloco. Deslocamentos acima de 12
    /// não existem no formato; o silício os trata como 9.
    pub fn decode(&mut self, nibble: u8, shift: u8, filter: u8) -> i16 {
        let shift = if shift > 12 { 9 } else { shift };
        let filter = (filter as usize).min(FILTER_OLD.len() - 1);

        // O nibble é um inteiro de 4 bits com sinal, alinhado ao topo de uma
        // palavra de 16 antes do deslocamento.
        let raw = i32::from(((nibble as i16) << 12) >> shift);
        let predicted =
            (self.old * FILTER_OLD[filter] + self.older * FILTER_OLDER[filter] + 32) >> 6;
        let sample = (raw + predicted).clamp(i16::MIN as i32, i16::MAX as i32);

        self.older = self.old;
        self.old = sample;
        sample as i16
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

/// Amostras que um bloco SPU-ADPCM carrega.
pub const SAMPLES_PER_BLOCK: usize = 28;

/// Bandeiras no segundo byte do cabeçalho de um bloco SPU-ADPCM.
pub mod flags {
    /// Fim do laço: marca `ENDX` e salta para o endereço de repetição.
    pub const LOOP_END: u8 = 1 << 0;
    /// Repetir. Sem ele, o fim do laço também força o release com volume zero.
    pub const LOOP_REPEAT: u8 = 1 << 1;
    /// Início do laço: guarda o endereço corrente como ponto de retorno.
    pub const LOOP_START: u8 = 1 << 2;
}

/// Decodifica um bloco de 16 bytes da SPU RAM.
///
/// Devolve as 28 amostras e as bandeiras do cabeçalho.
pub fn decode_spu_block(block: &[u8; 16], history: &mut History) -> ([i16; SAMPLES_PER_BLOCK], u8) {
    let shift = block[0] & 0x0F;
    let filter = block[0] >> 4;
    let mut samples = [0i16; SAMPLES_PER_BLOCK];

    for (index, sample) in samples.iter_mut().enumerate() {
        let byte = block[2 + index / 2];
        let nibble = if index % 2 == 0 {
            byte & 0x0F
        } else {
            byte >> 4
        };
        *sample = history.decode(nibble, shift, filter);
    }

    (samples, block[1])
}

/// Como um setor XA está codificado, lido do byte de "coding info".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct XaCoding {
    pub stereo: bool,
    /// Amostras por segundo do fluxo: 37800 ou 18900.
    pub sample_rate: u32,
    /// Amostras de 8 bits em vez de 4. Nenhum jogo usa, e o decodificador
    /// abaixo só sabe 4 — o formato existe no CD-XA, não no PSX.
    pub eight_bit: bool,
}

impl XaCoding {
    pub const fn from_byte(coding: u8) -> Self {
        Self {
            stereo: coding & 0x03 != 0,
            sample_rate: if coding & 0x0C != 0 { 18_900 } else { 37_800 },
            eight_bit: coding & 0x30 != 0,
        }
    }
}

/// Bytes de carga de um setor Form 2, que é onde o áudio XA mora.
pub const XA_SECTOR_BYTES: usize = 2324;
/// Um grupo de som: 16 bytes de parâmetros e 112 de amostras.
const XA_GROUP_BYTES: usize = 128;
/// Grupos de som num setor.
const XA_GROUPS: usize = 18;
/// Blocos dentro de um grupo, cada um pegando um nibble de cada palavra.
const XA_BLOCKS_PER_GROUP: usize = 8;

/// Decodifica os 2324 bytes de um setor XA em quadros estéreo.
///
/// Um setor mono devolve o mesmo valor nos dois canais, que é o que o mixer
/// da SPU recebe do CD.
///
/// Os oito blocos de um grupo não são consecutivos: cada um pega um nibble
/// fixo de cada palavra de 32 bits, e os parâmetros ficam nos bytes 4..12 do
/// cabeçalho — os outros oito são cópias redundantes. Ler os parâmetros da
/// posição errada faz o filtro divergir e saturar, o que é fácil de conferir
/// num setor real.
pub fn decode_xa_sector(
    data: &[u8],
    coding: XaCoding,
    history: &mut [History; 2],
    out: &mut Vec<(i16, i16)>,
) {
    if data.len() < XA_SECTOR_BYTES || coding.eight_bit {
        return;
    }

    for group in 0..XA_GROUPS {
        let base = group * XA_GROUP_BYTES;
        let parameters = &data[base + 4..base + 12];
        let words = &data[base + 16..base + XA_GROUP_BYTES];

        // Num grupo estéreo os blocos alternam entre os canais, e cada canal
        // guarda a sua previsão. Em mono cada bloco é um trecho seguido.
        let mut mono = [0i16; SAMPLES_PER_BLOCK];
        let mut left = [0i16; SAMPLES_PER_BLOCK];

        for (block, &parameter) in parameters.iter().enumerate().take(XA_BLOCKS_PER_GROUP) {
            let shift = parameter & 0x0F;
            let filter = parameter >> 4;
            let channel = if coding.stereo { block & 1 } else { 0 };

            let mut samples = [0i16; SAMPLES_PER_BLOCK];
            for (index, sample) in samples.iter_mut().enumerate() {
                let word = u32::from_le_bytes([
                    words[index * 4],
                    words[index * 4 + 1],
                    words[index * 4 + 2],
                    words[index * 4 + 3],
                ]);
                let nibble = ((word >> (block * 4)) & 0x0F) as u8;
                *sample = history[channel].decode(nibble, shift, filter);
            }

            if !coding.stereo {
                mono.copy_from_slice(&samples);
                out.extend(mono.iter().map(|&sample| (sample, sample)));
            } else if channel == 0 {
                left = samples;
            } else {
                out.extend(left.iter().zip(samples.iter()).map(|(&l, &r)| (l, r)));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_zero_is_just_the_shifted_nibble() {
        let mut history = History::default();
        // Nibble 1 com shift 12: 1 << 12 >> 12 = 1.
        assert_eq!(history.decode(1, 12, 0), 1);
        // Nibble 0xF é -1 com sinal.
        assert_eq!(history.decode(0x0F, 12, 0), -1);
    }

    #[test]
    fn the_prediction_uses_the_two_previous_samples() {
        let mut history = History::default();
        // Filtro 1: previsão = anterior * 60/64, sem termo penúltimo.
        let first = history.decode(1, 0, 1);
        assert_eq!(first, 4096);
        let second = history.decode(0, 0, 1);
        assert_eq!(i32::from(second), (4096i32 * 60 + 32) >> 6);
    }

    #[test]
    fn a_shift_above_twelve_behaves_as_nine() {
        let mut a = History::default();
        let mut b = History::default();
        assert_eq!(a.decode(1, 13, 0), b.decode(1, 9, 0));
    }

    #[test]
    fn coding_info_reads_stereo_and_sample_rate() {
        let mono = XaCoding::from_byte(0x00);
        assert!(!mono.stereo);
        assert_eq!(mono.sample_rate, 37_800);

        let stereo = XaCoding::from_byte(0x01);
        assert!(stereo.stereo);
        assert_eq!(stereo.sample_rate, 37_800);

        let half = XaCoding::from_byte(0x05);
        assert!(half.stereo);
        assert_eq!(half.sample_rate, 18_900);
    }

    #[test]
    fn a_stereo_sector_yields_one_frame_per_sample_pair() {
        let data = vec![0u8; XA_SECTOR_BYTES];
        let mut history = [History::default(); 2];
        let mut out = Vec::new();
        decode_xa_sector(&data, XaCoding::from_byte(0x01), &mut history, &mut out);
        // 18 grupos × 4 pares de blocos × 28 amostras.
        assert_eq!(out.len(), 18 * 4 * SAMPLES_PER_BLOCK);
    }

    #[test]
    fn a_mono_sector_yields_twice_as_many_frames() {
        let data = vec![0u8; XA_SECTOR_BYTES];
        let mut history = [History::default(); 2];
        let mut out = Vec::new();
        decode_xa_sector(&data, XaCoding::from_byte(0x00), &mut history, &mut out);
        assert_eq!(out.len(), 18 * 8 * SAMPLES_PER_BLOCK);
    }

    #[test]
    fn a_spu_block_decodes_twenty_eight_samples_and_its_flags() {
        let mut block = [0u8; 16];
        block[0] = 0x0C; // shift 12, filtro 0
        block[1] = flags::LOOP_END | flags::LOOP_REPEAT;
        block[2] = 0x21; // primeiras duas amostras: 1 e 2
        let mut history = History::default();
        let (samples, header) = decode_spu_block(&block, &mut history);
        assert_eq!(samples[0], 1);
        assert_eq!(samples[1], 2);
        assert_eq!(header, flags::LOOP_END | flags::LOOP_REPEAT);
    }
}
