//! Os três timers de propósito geral.
//!
//! Referência: PSX-SPX — "Timers".
//!
//! Cada timer ocupa `0x1F80_1100 + n*0x10`:
//! `+0` contador, `+4` modo, `+8` alvo. Todos são de 16 bits.
//!
//! **Escopo atual:** as fontes de clock derivadas do vídeo (dotclock e
//! hblank) usam uma aproximação por proporção de ciclos de CPU. A modelagem
//! exata depende do timing de scanline da GPU e é entrega do agente `@gpu`
//! em conjunto com o `@cpu`.

use crate::irq::{Interrupt, IrqController};

/// Quantidade de timers.
pub const TIMER_COUNT: usize = 3;

#[derive(Debug, Clone, Copy, Default)]
struct Timer {
    counter: u32,
    target: u32,
    mode: u32,
    /// Acumulador fracionário para fontes de clock mais lentas que a CPU.
    fraction: u32,
    reached_target: bool,
    reached_overflow: bool,
    /// O bit 10 é ativo-baixo: `true` aqui significa "IRQ ainda não disparada".
    irq_line_high: bool,
    /// No modo one-shot, a IRQ só pode ser gerada uma vez.
    one_shot_fired: bool,
}

impl Timer {
    const fn sync_enabled(&self) -> bool {
        self.mode & 1 != 0
    }

    const fn reset_on_target(&self) -> bool {
        self.mode & (1 << 3) != 0
    }

    const fn irq_on_target(&self) -> bool {
        self.mode & (1 << 4) != 0
    }

    const fn irq_on_overflow(&self) -> bool {
        self.mode & (1 << 5) != 0
    }

    const fn irq_repeats(&self) -> bool {
        self.mode & (1 << 6) != 0
    }

    const fn clock_source(&self) -> u32 {
        (self.mode >> 8) & 3
    }

    /// Leitura do registrador de modo. Os bits 11 e 12 zeram ao serem lidos.
    fn read_mode(&mut self) -> u32 {
        let value = (self.mode & 0x03FF)
            | ((self.irq_line_high as u32) << 10)
            | ((self.reached_target as u32) << 11)
            | ((self.reached_overflow as u32) << 12);
        self.reached_target = false;
        self.reached_overflow = false;
        value
    }

    fn write_mode(&mut self, value: u32) {
        self.mode = value & 0x03FF;
        // Escrever no modo sempre zera o contador.
        self.counter = 0;
        self.fraction = 0;
        self.irq_line_high = true;
        self.one_shot_fired = false;
    }
}

/// Bloco dos três timers.
#[derive(Debug, Clone)]
pub struct Timers {
    timers: [Timer; TIMER_COUNT],
}

impl Timers {
    pub fn new() -> Self {
        Self {
            timers: [Timer {
                irq_line_high: true,
                ..Timer::default()
            }; TIMER_COUNT],
        }
    }

    /// Avança os três timers em `cycles` ciclos de CPU.
    pub fn step(&mut self, cycles: u32, irq: &mut IrqController) {
        for index in 0..TIMER_COUNT {
            let ticks = self.ticks_for(index, cycles);
            if ticks > 0 {
                self.advance(index, ticks, irq);
            }
        }
    }

    /// Converte ciclos de CPU em ticks do timer conforme a fonte de clock.
    fn ticks_for(&mut self, index: usize, cycles: u32) -> u32 {
        let timer = &mut self.timers[index];
        let source = timer.clock_source();

        // PSX-SPX: a interpretação dos 2 bits muda por timer.
        let divider = match (index, source) {
            // Timer 0: dotclock nos modos 1 e 3. O dotclock é ~1/6 do clock da
            // CPU em 320 px, que é a resolução mais comum.
            (0, 1) | (0, 3) => 6,
            // Timer 1: hblank nos modos 1 e 3 — uma scanline tem ~2154 ciclos.
            (1, 1) | (1, 3) => 2154,
            // Timer 2: system clock / 8 nos modos 2 e 3.
            (2, 2) | (2, 3) => 8,
            _ => 1,
        };

        if divider == 1 {
            return cycles;
        }

        timer.fraction += cycles;
        let ticks = timer.fraction / divider;
        timer.fraction %= divider;
        ticks
    }

    fn advance(&mut self, index: usize, ticks: u32, irq: &mut IrqController) {
        let timer = &mut self.timers[index];

        // O modo de sincronização com vídeo pode pausar o contador; sem o
        // timing de scanline modelado, tratamos como livre e registramos a
        // limitação.
        let _ = timer.sync_enabled();

        let target = timer.target & 0xFFFF;
        let before = timer.counter;
        let mut counter = before + ticks;

        let mut hit_target = false;
        let mut hit_overflow = false;

        if target > 0 || timer.reset_on_target() {
            // Alvo zero é atingido a cada tick; caso contrário, é a travessia
            // do valor que conta.
            hit_target = (before < target && counter >= target) || (target == 0 && counter > 0);
        }

        if timer.reset_on_target() && hit_target {
            counter = if target == 0 {
                0
            } else {
                counter % (target + 1)
            };
        }

        if counter > 0xFFFF {
            hit_overflow = true;
            counter &= 0xFFFF;
        }

        timer.counter = counter;
        timer.reached_target |= hit_target;
        timer.reached_overflow |= hit_overflow;

        let should_irq =
            (hit_target && timer.irq_on_target()) || (hit_overflow && timer.irq_on_overflow());

        if should_irq && (timer.irq_repeats() || !timer.one_shot_fired) {
            timer.one_shot_fired = true;
            timer.irq_line_high = false;
            irq.raise(match index {
                0 => Interrupt::Timer0,
                1 => Interrupt::Timer1,
                _ => Interrupt::Timer2,
            });
            // No modo pulso a linha volta imediatamente ao repouso.
            timer.irq_line_high = true;
        }
    }

    /// Leitura de um registrador (offset dentro de `0x1F80_1100`).
    pub fn read(&mut self, offset: u32) -> u32 {
        let index = ((offset >> 4) & 3) as usize;
        if index >= TIMER_COUNT {
            return 0;
        }
        match offset & 0x0F {
            0x00 => self.timers[index].counter & 0xFFFF,
            0x04 => self.timers[index].read_mode(),
            0x08 => self.timers[index].target & 0xFFFF,
            _ => 0,
        }
    }

    /// Escrita num registrador (offset dentro de `0x1F80_1100`).
    pub fn write(&mut self, offset: u32, value: u32) {
        let index = ((offset >> 4) & 3) as usize;
        if index >= TIMER_COUNT {
            return;
        }
        match offset & 0x0F {
            0x00 => self.timers[index].counter = value & 0xFFFF,
            0x04 => self.timers[index].write_mode(value),
            0x08 => self.timers[index].target = value & 0xFFFF,
            _ => {}
        }
    }
}

impl Default for Timers {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counter_advances_with_the_system_clock() {
        let mut timers = Timers::new();
        let mut irq = IrqController::new();
        timers.step(100, &mut irq);
        assert_eq!(timers.read(0x00), 100);
    }

    #[test]
    fn writing_mode_resets_the_counter() {
        let mut timers = Timers::new();
        let mut irq = IrqController::new();
        timers.step(100, &mut irq);
        timers.write(0x04, 0);
        assert_eq!(timers.read(0x00), 0);
    }

    #[test]
    fn timer2_divides_the_clock_by_eight() {
        let mut timers = Timers::new();
        let mut irq = IrqController::new();
        // Timer 2 (offset 0x20), fonte 2 = system clock / 8.
        timers.write(0x24, 2 << 8);
        timers.step(16, &mut irq);
        assert_eq!(timers.read(0x20), 2);
        // A fração é preservada entre chamadas.
        timers.step(4, &mut irq);
        assert_eq!(timers.read(0x20), 2);
        timers.step(4, &mut irq);
        assert_eq!(timers.read(0x20), 3);
    }

    #[test]
    fn reaching_the_target_sets_the_sticky_bit_and_clears_on_read() {
        let mut timers = Timers::new();
        let mut irq = IrqController::new();
        timers.write(0x08, 50); // target
        timers.write(0x04, 0); // modo livre, sem reset
        timers.step(60, &mut irq);

        let mode = timers.read(0x04);
        assert_ne!(mode & (1 << 11), 0, "bit 11 sinaliza alvo atingido");
        let mode = timers.read(0x04);
        assert_eq!(mode & (1 << 11), 0, "e zera na leitura");
    }

    #[test]
    fn reset_on_target_wraps_the_counter() {
        let mut timers = Timers::new();
        let mut irq = IrqController::new();
        timers.write(0x08, 99);
        timers.write(0x04, 1 << 3); // reset ao atingir o alvo
        timers.step(150, &mut irq);
        assert_eq!(timers.read(0x00), 50, "150 % 100 = 50");
    }

    #[test]
    fn irq_on_target_raises_the_right_interrupt() {
        let mut timers = Timers::new();
        let mut irq = IrqController::new();
        irq.write_mask(0x07FF);

        timers.write(0x14, 0); // timer 1, modo limpo
        timers.write(0x18, 10); // target
        timers.write(0x14, (1 << 4) | (1 << 3)); // IRQ no alvo + reset
        timers.step(20, &mut irq);

        assert_ne!(irq.stat() & (1 << Interrupt::Timer1 as u16), 0);
    }

    #[test]
    fn one_shot_mode_fires_only_once() {
        let mut timers = Timers::new();
        let mut irq = IrqController::new();
        irq.write_mask(0x07FF);

        timers.write(0x08, 10);
        timers.write(0x04, (1 << 4) | (1 << 3)); // IRQ no alvo, sem repeat
        timers.step(11, &mut irq);
        assert_ne!(irq.stat() & 1 << Interrupt::Timer0 as u16, 0);

        // Limpa e roda de novo: sem repeat, não dispara.
        irq.write_stat(!(1 << Interrupt::Timer0 as u16));
        timers.step(100, &mut irq);
        assert_eq!(irq.stat() & 1 << Interrupt::Timer0 as u16, 0);
    }

    #[test]
    fn repeat_mode_fires_again() {
        let mut timers = Timers::new();
        let mut irq = IrqController::new();
        irq.write_mask(0x07FF);

        timers.write(0x08, 10);
        timers.write(0x04, (1 << 4) | (1 << 3) | (1 << 6)); // com repeat
        timers.step(11, &mut irq);
        irq.write_stat(!(1 << Interrupt::Timer0 as u16));

        timers.step(11, &mut irq);
        assert_ne!(irq.stat() & 1 << Interrupt::Timer0 as u16, 0);
    }

    #[test]
    fn counter_wraps_at_sixteen_bits() {
        let mut timers = Timers::new();
        let mut irq = IrqController::new();
        timers.write(0x00, 0xFFF0);
        timers.step(0x20, &mut irq);
        assert_eq!(timers.read(0x00), 0x10);
    }
}
