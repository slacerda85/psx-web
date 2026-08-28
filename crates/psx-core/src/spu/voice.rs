//! Uma das 24 vozes da SPU: leitura do ADPCM, tom e envelope.
//!
//! Referência: PSX-SPX — "SPU ADPCM Samples", "SPU ADPCM Pitch",
//! "SPU Volume and ADSR Generator".

use super::adpcm::{self, History, SAMPLES_PER_BLOCK};

/// Etapa do envelope. `Off` é a voz calada, antes de qualquer *key on*.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Off,
    Attack,
    Decay,
    Sustain,
    Release,
}

/// Parâmetros de uma rampa do envelope, já decodificados do registrador.
#[derive(Debug, Clone, Copy)]
struct Ramp {
    shift: u8,
    step: u8,
    exponential: bool,
    decreasing: bool,
}

impl Ramp {
    /// Um passo do envelope, conforme o pseudocódigo do PSX-SPX.
    ///
    /// Devolve o incremento do contador e o quanto somar ao nível. O contador
    /// existe porque as rampas lentas não mexem no nível a cada amostra: elas
    /// acumulam até transbordar o bit 15.
    fn advance(&self, level: i16) -> (u32, i32) {
        let mut step = 7 - i32::from(self.step);
        if self.decreasing {
            step = !step;
        }
        step <<= 11u8.saturating_sub(self.shift);

        let mut counter = 0x8000u32 >> self.shift.saturating_sub(11);

        if self.exponential && !self.decreasing && level > 0x6000 {
            // O "crescimento exponencial" é uma farsa do silício: ele apenas
            // desacelera a rampa linear depois de três quartos do volume.
            if self.shift < 10 {
                step >>= 2;
            } else if self.shift >= 11 {
                counter >>= 2;
            } else {
                step >>= 1;
                counter >>= 1;
            }
        } else if self.exponential && self.decreasing {
            // Deslocamento aritmético, e não divisão: para passo negativo a
            // divisão trunca **para zero** (-800 / 32768 == 0) e o nível para
            // de descer, deixando a voz presa soando baixo para sempre. O
            // deslocamento arredonda para baixo e sempre anda (-800 >> 15 == -1).
            step = (step * i32::from(level)) >> 15;
        }

        // Passo e shift todos em um nunca avançam o contador — é assim que o
        // software prende o envelope num nível.
        if self.step != 3 || self.shift != 0x1F {
            counter = counter.max(1);
        }

        (counter, step)
    }
}

/// Uma voz.
#[derive(Debug, Clone)]
pub struct Voice {
    /// `VxVOL` esquerdo e direito, como o software os escreveu.
    pub volume_left: u16,
    pub volume_right: u16,
    /// `VxPitch`: 0x1000 é 44100 Hz.
    pub pitch: u16,
    /// Endereço inicial do ADPCM, em unidades de 8 bytes.
    pub start_address: u16,
    /// Endereço de repetição, em unidades de 8 bytes.
    pub repeat_address: u16,
    /// `VxADSR`, os 32 bits do envelope.
    pub adsr: u32,
    /// Nível corrente do envelope.
    pub adsr_volume: i16,

    phase: Phase,
    adsr_counter: u32,
    /// Endereço corrente na SPU RAM, em halfwords.
    current_address: u32,
    /// Bloco decodificado e as três amostras anteriores, para interpolar.
    block: [i16; SAMPLES_PER_BLOCK],
    previous: [i16; 3],
    history: History,
    /// Posição dentro do bloco, com 12 bits de fração.
    counter: u32,
    /// O bloco corrente pediu para repetir a partir do endereço guardado.
    reached_end: bool,
    /// Última amostra produzida, para a modulação de tom da voz seguinte.
    pub last_sample: i16,
    /// Ainda não há bloco decodificado: o primeiro passo precisa buscar um.
    needs_block: bool,
}

impl Default for Voice {
    fn default() -> Self {
        Self::new()
    }
}

impl Voice {
    pub const fn new() -> Self {
        Self {
            volume_left: 0,
            volume_right: 0,
            pitch: 0,
            start_address: 0,
            repeat_address: 0,
            adsr: 0,
            adsr_volume: 0,
            phase: Phase::Off,
            adsr_counter: 0,
            current_address: 0,
            block: [0; SAMPLES_PER_BLOCK],
            previous: [0; 3],
            history: History::new_const(),
            counter: 0,
            reached_end: false,
            last_sample: 0,
            needs_block: true,
        }
    }

    pub const fn phase(&self) -> Phase {
        self.phase
    }

    pub const fn is_on(&self) -> bool {
        !matches!(self.phase, Phase::Off)
    }

    /// Endereço do bloco que a voz está lendo, em halfwords.
    ///
    /// É o que a IRQ de endereço da SPU compara.
    pub const fn current_block(&self) -> u32 {
        self.current_address
    }

    /// `ENDX`: o bloco marcado como fim de laço já foi alcançado.
    pub const fn reached_end(&self) -> bool {
        self.reached_end
    }

    /// *Key on*: recomeça do endereço inicial e entra no ataque.
    pub fn key_on(&mut self) {
        self.phase = Phase::Attack;
        self.adsr_volume = 0;
        self.adsr_counter = 0;
        self.current_address = u32::from(self.start_address) * 4;
        self.repeat_address = self.start_address;
        self.counter = 0;
        self.history.reset();
        self.block = [0; SAMPLES_PER_BLOCK];
        self.previous = [0; 3];
        self.reached_end = false;
        self.needs_block = true;
    }

    /// *Key off*: passa para o release, de onde o volume só desce.
    pub fn key_off(&mut self) {
        if self.phase != Phase::Off {
            self.phase = Phase::Release;
            self.adsr_counter = 0;
        }
    }

    /// Silencia a voz na hora, sem passar pelo release.
    fn silence(&mut self) {
        self.phase = Phase::Off;
        self.adsr_volume = 0;
        self.adsr_counter = 0;
    }

    /// A rampa da etapa corrente, extraída do registrador `VxADSR`.
    fn ramp(&self) -> Option<Ramp> {
        let low = self.adsr as u16;
        let high = (self.adsr >> 16) as u16;
        match self.phase {
            Phase::Off => None,
            Phase::Attack => Some(Ramp {
                shift: ((low >> 10) & 0x1F) as u8,
                step: ((low >> 8) & 0x03) as u8,
                exponential: low & 0x8000 != 0,
                decreasing: false,
            }),
            Phase::Decay => Some(Ramp {
                shift: ((low >> 4) & 0x0F) as u8,
                step: 0,
                exponential: true,
                decreasing: true,
            }),
            Phase::Sustain => Some(Ramp {
                shift: ((high >> 8) & 0x1F) as u8,
                step: ((high >> 6) & 0x03) as u8,
                exponential: high & 0x8000 != 0,
                decreasing: high & 0x4000 != 0,
            }),
            Phase::Release => Some(Ramp {
                shift: (high & 0x1F) as u8,
                step: 0,
                exponential: high & 0x0020 != 0,
                decreasing: true,
            }),
        }
    }

    /// Nível em que o decay entrega a voz ao sustain.
    fn sustain_level(&self) -> i32 {
        ((self.adsr & 0x0F) as i32 + 1) * 0x800
    }

    /// Avança o envelope em uma amostra.
    fn step_envelope(&mut self) {
        let Some(ramp) = self.ramp() else {
            return;
        };
        let (increment, step) = ramp.advance(self.adsr_volume);

        // Compara com o limiar em vez de testar o bit 15: um acúmulo que passe
        // de 0x10000 volta a ter o bit em zero, e o passo seria pulado.
        self.adsr_counter += increment;
        if self.adsr_counter < 0x8000 {
            return;
        }
        self.adsr_counter = 0;

        let level = i32::from(self.adsr_volume) + step;
        self.adsr_volume = if ramp.decreasing {
            level.max(0) as i16
        } else {
            level.clamp(0, 0x7FFF) as i16
        };

        match self.phase {
            Phase::Attack if self.adsr_volume == i16::MAX => self.phase = Phase::Decay,
            Phase::Decay if i32::from(self.adsr_volume) <= self.sustain_level() => {
                self.phase = Phase::Sustain
            }
            Phase::Release if self.adsr_volume == 0 => self.phase = Phase::Off,
            _ => {}
        }
    }

    /// Lê e decodifica o bloco de 16 bytes apontado por `current_address`.
    fn fetch_block(&mut self, ram: &[u16]) {
        let mut bytes = [0u8; 16];
        for (index, pair) in bytes.chunks_exact_mut(2).enumerate() {
            let word = ram[(self.current_address as usize + index) % ram.len()];
            pair.copy_from_slice(&word.to_le_bytes());
        }

        let (samples, flags) = adpcm::decode_spu_block(&bytes, &mut self.history);
        self.block = samples;

        if flags & adpcm::flags::LOOP_START != 0 {
            self.repeat_address = (self.current_address / 4) as u16;
        }
        if flags & adpcm::flags::LOOP_END != 0 {
            self.reached_end = true;
            self.current_address = u32::from(self.repeat_address) * 4;
            if flags & adpcm::flags::LOOP_REPEAT == 0 {
                // Fim sem repetição: o silício força o release com volume zero.
                self.silence();
            }
        } else {
            self.current_address = self.current_address.wrapping_add(8);
        }
        self.needs_block = false;
    }

    /// Produz uma amostra, já com o envelope aplicado.
    ///
    /// `modulation` é a amostra da voz anterior quando o `PMON` está ligado
    /// para esta — o tom passa a variar com a amplitude da vizinha.
    pub fn sample(&mut self, ram: &[u16], modulation: Option<i16>) -> i16 {
        // A voz não tem chave de liga-desliga: ela roda sempre, e o envelope é
        // quem a silencia. Parar o decodificador junto congelaria o endereço
        // corrente, e com ele a IRQ de endereço da SPU — que é como um jogo
        // descobre que um bloco de sample foi consumido.
        if self.needs_block {
            self.fetch_block(ram);
        }

        let index = (self.counter >> 12) as usize;
        let fraction = i32::from((self.counter & 0x0FFF) as u16);

        // Interpolação linear entre a amostra corrente e a anterior. O console
        // usa uma janela gaussiana de quatro pontos; a diferença é de timbre,
        // não de comportamento, e a tabela dela fica para quando o som já
        // estiver certo.
        let current = i32::from(self.block[index.min(SAMPLES_PER_BLOCK - 1)]);
        let previous = i32::from(self.previous[0]);
        let interpolated = previous + (((current - previous) * fraction) >> 12);

        let level = i32::from(self.adsr_volume);
        let output = ((interpolated * level) >> 15).clamp(i16::MIN as i32, i16::MAX as i32) as i16;

        self.step_envelope();
        self.advance_counter(modulation);
        self.last_sample = output;
        output
    }

    /// Avança o contador de tom e, ao passar do bloco, busca o próximo.
    fn advance_counter(&mut self, modulation: Option<i16>) {
        let mut step = u32::from(self.pitch);
        if let Some(factor) = modulation {
            // O fator vai de 0.00 a 1.99, centrado no silêncio da voz anterior.
            let factor = i32::from(factor) + 0x8000;
            step = (((step as i32) * factor) >> 15) as u32 & 0xFFFF;
        }
        // O console satura o passo em quatro amostras por ciclo.
        let step = step.min(0x4000);

        let before = (self.counter >> 12) as usize;
        self.counter = self.counter.wrapping_add(step);
        let after = (self.counter >> 12) as usize;

        for index in before..after.min(SAMPLES_PER_BLOCK) {
            self.previous[2] = self.previous[1];
            self.previous[1] = self.previous[0];
            self.previous[0] = self.block[index];
        }

        if after >= SAMPLES_PER_BLOCK {
            self.counter -= (SAMPLES_PER_BLOCK as u32) << 12;
            self.needs_block = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ram() -> Vec<u16> {
        // Um bloco que repete a si mesmo em silêncio: shift 12, sem bandeiras
        // de fim, todas as amostras zero.
        let mut ram = vec![0u16; 512];
        ram[0] = 0x000C;
        ram
    }

    #[test]
    fn key_on_starts_the_attack_from_zero() {
        let mut voice = Voice::new();
        voice.adsr = 0x0000_0FC0; // ataque rápido
        voice.key_on();
        assert_eq!(voice.phase(), Phase::Attack);
        assert_eq!(voice.adsr_volume, 0);
        assert!(voice.is_on());
    }

    #[test]
    fn a_silent_voice_outputs_nothing() {
        let mut voice = Voice::new();
        assert_eq!(voice.sample(&ram(), None), 0);
    }

    #[test]
    fn the_attack_climbs_to_full_and_hands_over_to_the_decay() {
        let mut voice = Voice::new();
        // Ataque linear no passo mais rápido, sustain no topo.
        voice.adsr = 0x0000_000F;
        voice.pitch = 0x1000;
        voice.key_on();
        let ram = ram();
        for _ in 0..4096 {
            voice.sample(&ram, None);
            if voice.phase() != Phase::Attack {
                break;
            }
        }
        assert_ne!(voice.phase(), Phase::Attack, "o ataque precisa terminar");
        assert!(voice.adsr_volume > 0x7000);
    }

    #[test]
    fn key_off_moves_to_release_and_eventually_silences() {
        let mut voice = Voice::new();
        voice.adsr = 0x0000_000F;
        voice.pitch = 0x1000;
        voice.key_on();
        let ram = ram();
        voice.sample(&ram, None);
        voice.key_off();
        assert_eq!(voice.phase(), Phase::Release);
        for _ in 0..200_000 {
            voice.sample(&ram, None);
            if !voice.is_on() {
                break;
            }
        }
        assert!(!voice.is_on(), "o release precisa chegar a zero");
        assert_eq!(voice.adsr_volume, 0);
    }

    #[test]
    fn a_block_with_loop_end_without_repeat_silences_the_voice() {
        let mut ram = vec![0u16; 512];
        // shift 12, bandeira de fim de laço sem repetição.
        ram[0] = 0x000C | ((adpcm::flags::LOOP_END as u16) << 8);
        let mut voice = Voice::new();
        voice.adsr = 0x0000_000F;
        voice.pitch = 0x1000;
        voice.key_on();
        for _ in 0..SAMPLES_PER_BLOCK + 2 {
            voice.sample(&ram, None);
        }
        assert!(voice.reached_end());
        assert!(!voice.is_on(), "fim sem repetição cala a voz");
    }

    #[test]
    fn a_pitch_of_zero_never_leaves_the_first_sample() {
        let mut voice = Voice::new();
        voice.adsr = 0x0000_000F;
        voice.pitch = 0;
        voice.key_on();
        let ram = ram();
        for _ in 0..100 {
            voice.sample(&ram, None);
        }
        assert!(!voice.needs_block, "sem tom o bloco não avança");
    }
}
