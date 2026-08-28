//! SPU — Sound Processing Unit.
//!
//! Referência: PSX-SPX — "Sound Processing Unit (SPU)", "SPU Registers".
//!
//! **Escopo atual:** banco de registradores, SPU RAM de 512 KB, transferências
//! por I/O e por DMA, as 24 vozes com ADPCM, envelope ADSR, tom com
//! interpolação e modulação, ruído, `ENDX`, a IRQ de endereço, e a entrada de
//! áudio do CD (XA-ADPCM e CD-DA) misturada à saída.
//!
//! **Ainda não implementado:** reverb e a janela gaussiana de quatro pontos na
//! interpolação. Nenhum dos dois muda o comportamento observável do jogo — o
//! primeiro é um efeito, o segundo é timbre.

pub mod adpcm;
pub mod voice;

use std::collections::VecDeque;

pub use adpcm::XaCoding;
pub use voice::Voice;

/// Tamanho da SPU RAM.
pub const SPU_RAM_SIZE: usize = 512 * 1024;
/// Frequência de saída do SPU.
pub const SAMPLE_RATE: u32 = 44_100;
/// Vozes que o hardware tem.
pub const VOICES: usize = 24;
/// Amostras (estéreo intercalado) que o ring buffer comporta.
const RING_CAPACITY: usize = SAMPLE_RATE as usize; // ~1 s de folga

/// Quadros de áudio de CD guardados à espera de serem misturados.
///
/// Um setor XA rende 2016 quadros de uma vez, e o drive entrega até 150
/// setores por segundo enquanto a mixagem consome 44100 quadros. A fila
/// absorve essa diferença de granularidade; passar disso significa que
/// ninguém está consumindo, e aí o mais antigo cai.
const CD_QUEUE_LIMIT: usize = SAMPLE_RATE as usize;

/// O SPU.
pub struct Spu {
    /// `0x1F80_1C00..0x1F80_1E00` — 256 registradores de 16 bits.
    registers: [u16; 0x100],
    ram: Box<[u16]>,
    voices: [Voice; VOICES],
    /// Endereço corrente de transferência, em halfwords.
    transfer_address: u32,
    /// Bit a bit, quais vozes já alcançaram o fim do laço (`ENDX`).
    endx: u32,
    /// Registrador de deslocamento do gerador de ruído.
    noise: u32,
    noise_counter: u32,
    /// A IRQ de endereço já disparou e ainda não foi reconhecida.
    irq_flag: bool,

    /// Áudio vindo do CD, na taxa do fluxo, à espera de reamostragem.
    cd_queue: VecDeque<(i16, i16)>,
    /// Taxa do fluxo corrente do CD.
    cd_rate: u32,
    /// Posição fracionária dentro de `cd_queue`, em 1/65536 de amostra.
    cd_fraction: u32,

    /// Buffer circular de saída, estéreo intercalado (L, R, L, R, ...).
    ring: Box<[i16]>,
    write_cursor: usize,
    read_cursor: usize,
    /// Resto fracionário de ciclos de CPU ainda não convertidos em amostras.
    cycle_fraction: u32,
}

/// Índices dos registradores globais, em halfwords a partir de `0x1F80_1C00`.
mod reg {
    pub const MAIN_VOLUME_LEFT: usize = 0x180 / 2;
    pub const KEY_ON: usize = 0x188 / 2;
    pub const KEY_OFF: usize = 0x18C / 2;
    pub const PITCH_MODULATION: usize = 0x190 / 2;
    pub const NOISE_ENABLE: usize = 0x194 / 2;
    pub const ENDX: usize = 0x19C / 2;
    pub const IRQ_ADDRESS: usize = 0x1A4 / 2;
    pub const TRANSFER_ADDRESS: usize = 0x1A6 / 2;
    pub const TRANSFER_FIFO: usize = 0x1A8 / 2;
    pub const CONTROL: usize = 0x1AA / 2;
    pub const STATUS: usize = 0x1AE / 2;
    pub const CD_VOLUME_LEFT: usize = 0x1B0 / 2;
    pub const CD_VOLUME_RIGHT: usize = 0x1B2 / 2;
    /// Primeiro índice fora do bloco das 24 vozes.
    pub const VOICE_END: usize = VOICES * 8;
    use super::VOICES;
}

/// Bits de `SPUCNT` (`0x1F80_1DAA`).
mod control {
    /// Áudio de CD misturado à saída.
    pub const CD_AUDIO: u16 = 1 << 0;
    /// A IRQ de endereço está armada.
    pub const IRQ_ENABLE: u16 = 1 << 6;
    /// SPU ligada.
    pub const ENABLE: u16 = 1 << 15;
    /// Sem isso a saída é silêncio, por mais que as vozes toquem.
    pub const UNMUTE: u16 = 1 << 14;
}

/// Ciclos de CPU por amostra de áudio: 33868800 / 44100 = 768, exato.
const CYCLES_PER_SAMPLE: u32 = crate::CPU_CLOCK_HZ / SAMPLE_RATE;

impl Spu {
    pub fn new() -> Self {
        Self {
            registers: [0; 0x100],
            ram: vec![0; SPU_RAM_SIZE / 2].into_boxed_slice(),
            voices: [const { Voice::new() }; VOICES],
            transfer_address: 0,
            endx: 0,
            noise: 1,
            noise_counter: 0,
            irq_flag: false,
            cd_queue: VecDeque::new(),
            cd_rate: 44_100,
            cd_fraction: 0,
            ring: vec![0; RING_CAPACITY * 2].into_boxed_slice(),
            write_cursor: 0,
            read_cursor: 0,
            cycle_fraction: 0,
        }
    }

    /// Avança o SPU, produzindo amostras.
    pub fn step(&mut self, cycles: u32) {
        self.cycle_fraction += cycles;
        let samples = self.cycle_fraction / CYCLES_PER_SAMPLE;
        self.cycle_fraction %= CYCLES_PER_SAMPLE;

        for _ in 0..samples {
            let (left, right) = self.mix_one();
            self.push_sample(left, right);
        }
    }

    /// Lê um par de registradores de 16 bits como um só de 32.
    fn register32(&self, index: usize) -> u32 {
        u32::from(self.registers[index]) | (u32::from(self.registers[index + 1]) << 16)
    }

    /// Mistura uma amostra: as 24 vozes mais a entrada do CD.
    fn mix_one(&mut self) -> (i16, i16) {
        self.step_noise();

        let control = self.registers[reg::CONTROL];
        let modulation = self.register32(reg::PITCH_MODULATION);
        let noise_enable = self.register32(reg::NOISE_ENABLE);

        let mut left = 0i32;
        let mut right = 0i32;
        let mut previous = 0i16;

        for index in 0..VOICES {
            let modulated = if index > 0 && modulation & (1 << index) != 0 {
                Some(previous)
            } else {
                None
            };

            // A voz decodifica o ADPCM mesmo em modo ruído: o endereço precisa
            // andar, senão a IRQ de endereço nunca dispara.
            let sample = {
                let ram = &self.ram;
                self.voices[index].sample(ram, modulated)
            };
            let sample = if noise_enable & (1 << index) != 0 {
                let level = i32::from(self.voices[index].adsr_volume);
                (((self.noise as i16 as i32) * level) >> 15) as i16
            } else {
                sample
            };

            previous = sample;
            left += apply_volume(sample, self.voices[index].volume_left);
            right += apply_volume(sample, self.voices[index].volume_right);

            if self.voices[index].reached_end() {
                self.endx |= 1 << index;
            }
        }

        if control & control::CD_AUDIO != 0 {
            let (cd_left, cd_right) = self.next_cd_frame();
            left += apply_volume(cd_left, self.registers[reg::CD_VOLUME_LEFT]);
            right += apply_volume(cd_right, self.registers[reg::CD_VOLUME_RIGHT]);
        } else {
            // Mesmo mudo o decodificador continua consumindo o fluxo: o CD não
            // para de girar porque o mixer está desligado.
            self.next_cd_frame();
        }

        self.check_irq_address();

        if control & (control::ENABLE | control::UNMUTE) != (control::ENABLE | control::UNMUTE) {
            return (0, 0);
        }

        let main_left = self.registers[reg::MAIN_VOLUME_LEFT];
        let main_right = self.registers[reg::MAIN_VOLUME_LEFT + 1];
        (
            saturate(apply_volume(saturate(left), main_left)),
            saturate(apply_volume(saturate(right), main_right)),
        )
    }

    /// Um passo do gerador de ruído.
    ///
    /// O período vem dos bits 8..13 do `SPUCNT` e é compartilhado por todas as
    /// vozes em modo ruído.
    fn step_noise(&mut self) {
        let control = self.registers[reg::CONTROL];
        let shift = (control >> 10) & 0x0F;
        let step = (control >> 8) & 0x03;
        let period = (0x8000u32 >> shift).max(1) * (4 + u32::from(step));

        self.noise_counter += 1;
        if self.noise_counter < period.max(1) {
            return;
        }
        self.noise_counter = 0;
        let bit =
            ((self.noise >> 15) ^ (self.noise >> 12) ^ (self.noise >> 11) ^ (self.noise >> 10)) & 1;
        self.noise = ((self.noise << 1) | bit) & 0xFFFF;
    }

    /// O próximo quadro de áudio do CD, reamostrado para 44100 Hz.
    fn next_cd_frame(&mut self) -> (i16, i16) {
        let Some(&frame) = self.cd_queue.front() else {
            return (0, 0);
        };
        // Quantas amostras da fonte cabem numa da saída, em 1/65536.
        //
        // A precisão importa: com 16 avos, um fluxo de 37800 Hz era consumido
        // 5% devagar demais e um de 18900 Hz 12,5%. A fila enchia até o teto e
        // passava a descartar blocos inteiros — o que se ouve como áudio
        // acelerado e picotado.
        self.cd_fraction += (self.cd_rate << 16) / SAMPLE_RATE;
        while self.cd_fraction >= 1 << 16 {
            self.cd_fraction -= 1 << 16;
            self.cd_queue.pop_front();
        }
        frame
    }

    /// Recebe áudio decodificado do CD-ROM.
    pub fn push_cd_audio(&mut self, frames: &[(i16, i16)], rate: u32) {
        if rate != self.cd_rate {
            self.cd_rate = rate;
            self.cd_fraction = 0;
        }
        self.cd_queue.extend(frames.iter().copied());
        while self.cd_queue.len() > CD_QUEUE_LIMIT {
            self.cd_queue.pop_front();
        }
    }

    /// `true` enquanto houver áudio de CD à espera de mixagem.
    ///
    /// É o que o `ADPBUSY` do CD-ROM reporta: o decodificador está ocupado.
    pub fn cd_audio_pending(&self) -> bool {
        !self.cd_queue.is_empty()
    }

    /// Dispara a IRQ da SPU quando alguma voz passa pelo endereço vigiado.
    fn check_irq_address(&mut self) {
        if self.registers[reg::CONTROL] & control::IRQ_ENABLE == 0 {
            self.irq_flag = false;
            return;
        }
        let watched = u32::from(self.registers[reg::IRQ_ADDRESS]) * 4;
        for voice in &self.voices {
            if voice.is_on() && voice.current_block() == watched & !7 {
                self.irq_flag = true;
                return;
            }
        }
    }

    /// A SPU está pedindo interrupção?
    pub const fn irq_pending(&self) -> bool {
        self.irq_flag
    }

    fn push_sample(&mut self, left: i16, right: i16) {
        let next = (self.write_cursor + 2) % self.ring.len();
        if next == self.read_cursor {
            // Buffer cheio: descarta a amostra mais antiga em vez de bloquear.
            self.read_cursor = (self.read_cursor + 2) % self.ring.len();
        }
        self.ring[self.write_cursor] = left;
        self.ring[self.write_cursor + 1] = right;
        self.write_cursor = next;
    }

    /// Copia até `out.len() / 2` frames para `out` e devolve quantas amostras
    /// (valores individuais) foram escritas.
    pub fn drain_samples(&mut self, out: &mut [i16]) -> usize {
        let mut written = 0;
        while written + 1 < out.len() && self.read_cursor != self.write_cursor {
            out[written] = self.ring[self.read_cursor];
            out[written + 1] = self.ring[self.read_cursor + 1];
            self.read_cursor = (self.read_cursor + 2) % self.ring.len();
            written += 2;
        }
        written
    }

    /// Quantas amostras estão disponíveis para consumo.
    pub fn queued_samples(&self) -> usize {
        if self.write_cursor >= self.read_cursor {
            self.write_cursor - self.read_cursor
        } else {
            self.ring.len() - self.read_cursor + self.write_cursor
        }
    }

    /// Leitura de 16 bits (offset dentro de `0x1F80_1C00`).
    pub fn read(&mut self, offset: u32) -> u16 {
        let index = ((offset >> 1) & 0xFF) as usize;
        match index {
            reg::STATUS => self.status(),
            reg::ENDX => self.endx as u16,
            index if index == reg::ENDX + 1 => (self.endx >> 16) as u16,
            reg::TRANSFER_FIFO => {
                let value = self.ram[(self.transfer_address as usize) % self.ram.len()];
                self.transfer_address = self.transfer_address.wrapping_add(1);
                value
            }
            index if index < reg::VOICE_END => self.read_voice(index),
            _ => self.registers[index],
        }
    }

    /// Escrita de 16 bits (offset dentro de `0x1F80_1C00`).
    pub fn write(&mut self, offset: u32, value: u16) {
        let index = ((offset >> 1) & 0xFF) as usize;
        match index {
            reg::TRANSFER_ADDRESS => {
                self.registers[index] = value;
                // O registrador guarda o endereço dividido por 8 (em bytes),
                // o que dá 4 halfwords por unidade.
                self.transfer_address = u32::from(value) * 4;
            }
            reg::TRANSFER_FIFO => self.write_ram(value),
            reg::KEY_ON => self.key_on(u32::from(value), 0),
            index if index == reg::KEY_ON + 1 => self.key_on(u32::from(value) << 16, 16),
            reg::KEY_OFF => self.key_off(u32::from(value)),
            index if index == reg::KEY_OFF + 1 => self.key_off(u32::from(value) << 16),
            // `ENDX` é limpo por escrita, não escrito.
            reg::ENDX => self.endx &= 0xFFFF_0000,
            index if index == reg::ENDX + 1 => self.endx &= 0x0000_FFFF,
            reg::CONTROL => {
                self.registers[index] = value;
                if value & control::IRQ_ENABLE == 0 {
                    self.irq_flag = false;
                }
            }
            // SPUSTAT é somente leitura.
            reg::STATUS => {}
            index if index < reg::VOICE_END => self.write_voice(index, value),
            _ => self.registers[index] = value,
        }
    }

    fn read_voice(&self, index: usize) -> u16 {
        let voice = &self.voices[index / 8];
        match index % 8 {
            0 => voice.volume_left,
            1 => voice.volume_right,
            2 => voice.pitch,
            3 => voice.start_address,
            4 => voice.adsr as u16,
            5 => (voice.adsr >> 16) as u16,
            6 => voice.adsr_volume as u16,
            _ => voice.repeat_address,
        }
    }

    fn write_voice(&mut self, index: usize, value: u16) {
        let voice = &mut self.voices[index / 8];
        match index % 8 {
            0 => voice.volume_left = value,
            1 => voice.volume_right = value,
            2 => voice.pitch = value,
            3 => voice.start_address = value,
            4 => voice.adsr = (voice.adsr & 0xFFFF_0000) | u32::from(value),
            5 => voice.adsr = (voice.adsr & 0x0000_FFFF) | (u32::from(value) << 16),
            6 => voice.adsr_volume = value as i16,
            _ => voice.repeat_address = value,
        }
    }

    fn key_on(&mut self, mask: u32, base: usize) {
        let _ = base;
        for index in 0..VOICES {
            if mask & (1 << index) != 0 {
                self.voices[index].key_on();
                // O *key on* limpa o `ENDX` da voz.
                self.endx &= !(1 << index);
            }
        }
    }

    fn key_off(&mut self, mask: u32) {
        for index in 0..VOICES {
            if mask & (1 << index) != 0 {
                self.voices[index].key_off();
            }
        }
    }

    fn write_ram(&mut self, value: u16) {
        let address = (self.transfer_address as usize) % self.ram.len();
        self.ram[address] = value;
        self.transfer_address = self.transfer_address.wrapping_add(1);
    }

    /// `SPUSTAT` (`0x1F80_1DAE`).
    ///
    /// Os bits 0..5 espelham `SPUCNT`; o BIOS fica em loop até o espelho
    /// bater com o que escreveu. O bit 6 é a IRQ pendente.
    fn status(&self) -> u16 {
        let mut value = self.registers[reg::CONTROL] & 0x003F;
        if self.irq_flag {
            value |= 1 << 6;
        }
        // Bit 10 (transferência ocupada) fica em zero porque as nossas são
        // instantâneas, e com ele em um o software esperaria para sempre.
        value
    }

    /// Transferência de DMA (canal 4), RAM → SPU.
    pub fn dma_write(&mut self, value: u32) {
        self.write_ram(value as u16);
        self.write_ram((value >> 16) as u16);
    }

    /// Transferência de DMA (canal 4), SPU → RAM.
    pub fn dma_read(&mut self) -> u32 {
        let low = self.ram[(self.transfer_address as usize) % self.ram.len()] as u32;
        self.transfer_address = self.transfer_address.wrapping_add(1);
        let high = self.ram[(self.transfer_address as usize) % self.ram.len()] as u32;
        self.transfer_address = self.transfer_address.wrapping_add(1);
        low | (high << 16)
    }

    pub fn ram(&self) -> &[u16] {
        &self.ram
    }

    /// Quantas vozes estão soando, para diagnóstico.
    pub fn active_voices(&self) -> usize {
        self.voices.iter().filter(|voice| voice.is_on()).count()
    }
}

/// Aplica um volume de voz ou principal a uma amostra.
///
/// No modo fixo o registrador é o volume dividido por dois. O modo de varredura
/// (bit 15) faz o volume subir ou descer sozinho ao longo do tempo; enquanto a
/// varredura não existe, usar o nível corrente é melhor que ignorar o
/// registrador — um jogo que só usa varredura ficaria mudo.
fn apply_volume(sample: i16, volume: u16) -> i32 {
    let level = if volume & 0x8000 != 0 {
        // Varredura: aproxima pelo meio da escala.
        0x3FFF
    } else {
        i32::from(((volume << 1) as i16) >> 1) * 2
    };
    (i32::from(sample) * level) >> 15
}

fn saturate(value: i32) -> i16 {
    value.clamp(i16::MIN as i32, i16::MAX as i32) as i16
}

impl Default for Spu {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spustat_mirrors_the_low_bits_of_spucnt() {
        let mut spu = Spu::new();
        spu.write(0x1AA, 0x8025);
        assert_eq!(spu.read(0x1AE) & 0x3F, 0x25);
    }

    #[test]
    fn spustat_is_read_only() {
        let mut spu = Spu::new();
        spu.write(0x1AE, 0xFFFF);
        assert_eq!(spu.read(0x1AE), 0);
    }

    #[test]
    fn transfer_writes_land_in_spu_ram() {
        let mut spu = Spu::new();
        spu.write(0x1A6, 0x0100); // endereço
        spu.write(0x1A8, 0xAAAA);
        spu.write(0x1A8, 0xBBBB);

        // Reposiciona e lê de volta.
        spu.write(0x1A6, 0x0100);
        assert_eq!(spu.read(0x1A8), 0xAAAA);
        assert_eq!(spu.read(0x1A8), 0xBBBB);
    }

    #[test]
    fn dma_moves_two_halfwords_per_word() {
        let mut spu = Spu::new();
        spu.write(0x1A6, 0x0200);
        spu.dma_write(0xBBBB_AAAA);

        spu.write(0x1A6, 0x0200);
        assert_eq!(spu.dma_read(), 0xBBBB_AAAA);
    }

    #[test]
    fn one_sample_is_produced_every_768_cycles() {
        let mut spu = Spu::new();
        assert_eq!(CYCLES_PER_SAMPLE, 768, "33868800 / 44100 é exato");

        spu.step(767);
        assert_eq!(spu.queued_samples(), 0);
        spu.step(1);
        assert_eq!(spu.queued_samples(), 2, "um frame estéreo");
    }

    #[test]
    fn drain_empties_the_ring() {
        let mut spu = Spu::new();
        spu.step(CYCLES_PER_SAMPLE * 10);
        assert_eq!(spu.queued_samples(), 20);

        let mut out = [0i16; 64];
        let written = spu.drain_samples(&mut out);
        assert_eq!(written, 20);
        assert_eq!(spu.queued_samples(), 0);
        assert_eq!(spu.drain_samples(&mut out), 0);
    }

    #[test]
    fn ring_overflow_drops_the_oldest_frames_instead_of_blocking() {
        let mut spu = Spu::new();
        // Mais amostras do que o buffer comporta.
        spu.step(CYCLES_PER_SAMPLE * (RING_CAPACITY as u32 + 100));
        assert!(spu.queued_samples() <= spu.ring.len());
    }

    /// Liga a SPU e põe volume máximo em tudo.
    fn unmuted() -> Spu {
        let mut spu = Spu::new();
        spu.write(0x1AA, control::ENABLE | control::UNMUTE | control::CD_AUDIO);
        spu.write(0x180, 0x3FFF); // volume principal esquerdo
        spu.write(0x182, 0x3FFF); // direito
        spu
    }

    #[test]
    fn key_on_lights_a_voice_and_key_off_puts_it_in_release() {
        let mut spu = unmuted();
        spu.write(0x188, 0x0001); // key on na voz 0
        assert_eq!(spu.active_voices(), 1);

        spu.write(0x18C, 0x0001); // key off
        assert_eq!(spu.voices[0].phase(), voice::Phase::Release);
    }

    #[test]
    fn key_on_reaches_the_upper_voices() {
        let mut spu = unmuted();
        spu.write(0x18A, 0x0080); // voz 23
        assert!(spu.voices[23].is_on());
        assert_eq!(spu.active_voices(), 1);
    }

    #[test]
    fn endx_is_cleared_by_writing_and_by_key_on() {
        let mut spu = unmuted();
        spu.endx = 0x00FF_FFFF;
        spu.write(0x19C, 0xFFFF);
        assert_eq!(spu.endx & 0xFFFF, 0);
        spu.endx = 0x00FF_FFFF;
        spu.write(0x188, 0x0001);
        assert_eq!(spu.endx & 1, 0);
    }

    #[test]
    fn a_muted_spu_outputs_silence_even_with_a_voice_playing() {
        let mut spu = unmuted();
        // Desliga o unmute, mantendo a SPU ligada.
        spu.write(0x1AA, control::ENABLE);
        spu.write(0x188, 0x0001);
        spu.step(CYCLES_PER_SAMPLE * 4);
        let mut out = [0i16; 16];
        let written = spu.drain_samples(&mut out);
        assert!(out[..written].iter().all(|&sample| sample == 0));
    }

    #[test]
    fn cd_audio_reaches_the_output() {
        let mut spu = unmuted();
        spu.write(0x1B0, 0x3FFF); // volume do CD à esquerda
        spu.write(0x1B2, 0x3FFF);
        spu.push_cd_audio(&[(10_000, -10_000); 256], 44_100);
        assert!(spu.cd_audio_pending());

        spu.step(CYCLES_PER_SAMPLE * 8);
        let mut out = [0i16; 32];
        let written = spu.drain_samples(&mut out);
        assert!(written > 0);
        assert!(
            out[..written].iter().any(|&sample| sample != 0),
            "o áudio do CD precisa aparecer na saída"
        );
    }

    #[test]
    fn cd_audio_is_dropped_when_the_mixer_bit_is_off() {
        let mut spu = unmuted();
        spu.write(0x1AA, control::ENABLE | control::UNMUTE); // sem CD_AUDIO
        spu.write(0x1B0, 0x3FFF);
        spu.write(0x1B2, 0x3FFF);
        spu.push_cd_audio(&[(20_000, 20_000); 256], 44_100);

        spu.step(CYCLES_PER_SAMPLE * 8);
        let mut out = [0i16; 32];
        let written = spu.drain_samples(&mut out);
        assert!(out[..written].iter().all(|&sample| sample == 0));
    }

    #[test]
    fn a_slower_cd_stream_is_consumed_at_its_own_rate() {
        for (rate, expected) in [(37_800u32, 857usize), (18_900, 428), (44_100, 1000)] {
            let mut spu = unmuted();
            spu.push_cd_audio(&[(1, 1); 1200], rate);
            let before = spu.cd_queue.len();
            for _ in 0..1000 {
                spu.next_cd_frame();
            }
            let consumed = before - spu.cd_queue.len();
            // Mil amostras de saída a 44100 Hz consomem rate/44.1 da fonte. Um
            // erro de 1% aqui vira fila estourada e áudio picotado.
            assert!(
                consumed.abs_diff(expected) <= 2,
                "fonte de {rate} Hz: consumiu {consumed}, esperado {expected}"
            );
        }
    }

    #[test]
    fn the_irq_flag_only_arms_when_the_control_bit_is_set() {
        let mut spu = unmuted();
        spu.irq_flag = true;
        spu.write(0x1AA, control::ENABLE); // sem IRQ_ENABLE
        assert!(!spu.irq_pending());
        assert_eq!(spu.read(0x1AE) & (1 << 6), 0);
    }
}
