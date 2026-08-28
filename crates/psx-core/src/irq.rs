//! Controlador de interrupções.
//!
//! Referência: PSX-SPX — "Interrupts".
//!
//! - `I_STAT` (`0x1F80_1070`): flags pendentes. Escrever `0` num bit o limpa;
//!   escrever `1` o **mantém**. Ou seja: `stat &= value`.
//! - `I_MASK` (`0x1F80_1074`): habilitação por fonte.
//! - A linha de IRQ para a CPU (COP0 `Cause.IP2`) fica ativa enquanto
//!   `stat & mask != 0`.

/// Fontes de interrupção, na ordem dos bits de `I_STAT`/`I_MASK`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum Interrupt {
    /// Retraço vertical (fim do display area).
    VBlank = 0,
    /// GPU — apenas para o comando GP0(0x1F).
    Gpu = 1,
    CdRom = 2,
    Dma = 3,
    Timer0 = 4,
    Timer1 = 5,
    Timer2 = 6,
    /// SIO0 — controllers e memory cards.
    ControllerAndMemoryCard = 7,
    Sio = 8,
    Spu = 9,
    /// Lightpen / controller irq10.
    Lightpen = 10,
}

/// Estado do controlador de interrupções.
#[derive(Debug, Clone, Copy, Default)]
pub struct IrqController {
    stat: u16,
    mask: u16,
    /// Nível corrente de cada linha física.
    ///
    /// `I_STAT` guarda a **borda de subida**, não o nível: um periférico que
    /// mantém a linha alta não gera flag nova. Guardar o nível aqui é o que
    /// permite ao periférico apenas publicar o seu estado, sem ter que
    /// descobrir sozinho quando houve transição.
    level: u16,
}

impl IrqController {
    pub const fn new() -> Self {
        Self {
            stat: 0,
            mask: 0,
            level: 0,
        }
    }

    /// Sinaliza uma interrupção (seta o bit em `I_STAT`).
    ///
    /// Para uma fonte de pulso, como o VBlank. Uma fonte que mantém uma linha
    /// enquanto tem trabalho pendente deve usar [`Self::set_level`].
    #[inline]
    pub fn raise(&mut self, irq: Interrupt) {
        self.stat |= 1 << (irq as u16);
    }

    /// Publica o nível da linha de uma fonte.
    ///
    /// A flag em `I_STAT` só sobe na transição de baixo para alto. Uma fonte
    /// que já está alta e continua alta não gera flag nova — mas uma que
    /// abaixa e volta a subir gera.
    ///
    /// É o que permite a um periférico levantar a interrupção quando o
    /// software **habilita** uma fonte cuja condição já era verdadeira. Com o
    /// pulso no instante do evento, essa interrupção se perdia: o CD-ROM
    /// ficava com a flag acesa, sem IRQ, e parava de entregar respostas.
    #[inline]
    pub fn set_level(&mut self, irq: Interrupt, high: bool) {
        let bit = 1 << (irq as u16);
        if high {
            if self.level & bit == 0 {
                self.level |= bit;
                self.stat |= bit;
            }
        } else {
            self.level &= !bit;
        }
    }

    /// `true` enquanto houver interrupção pendente **e** habilitada.
    #[inline]
    pub const fn is_pending(&self) -> bool {
        self.stat & self.mask != 0
    }

    #[inline]
    pub const fn stat(&self) -> u16 {
        self.stat
    }

    #[inline]
    pub const fn mask(&self) -> u16 {
        self.mask
    }

    /// Escrita em `I_STAT`: acknowledge. Bits escritos como `0` são limpos.
    #[inline]
    pub fn write_stat(&mut self, value: u16) {
        self.stat &= value;
    }

    /// Escrita em `I_MASK`.
    #[inline]
    pub fn write_mask(&mut self, value: u16) {
        self.mask = value & 0x07FF;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_requires_both_stat_and_mask() {
        let mut irq = IrqController::new();
        assert!(!irq.is_pending());

        irq.raise(Interrupt::VBlank);
        // Sem máscara, a linha para a CPU continua baixa.
        assert!(!irq.is_pending());

        irq.write_mask(1 << Interrupt::VBlank as u16);
        assert!(irq.is_pending());
    }

    #[test]
    fn writing_zero_acknowledges_the_bit() {
        let mut irq = IrqController::new();
        irq.write_mask(0x07FF);
        irq.raise(Interrupt::CdRom);
        irq.raise(Interrupt::Timer1);
        assert_eq!(irq.stat(), (1 << 2) | (1 << 5));

        // Acknowledge só do CD-ROM: escrevemos 0 no bit 2 e 1 nos demais.
        irq.write_stat(!(1 << 2));
        assert_eq!(irq.stat(), 1 << 5);
        assert!(irq.is_pending());

        irq.write_stat(!(1 << 5));
        assert_eq!(irq.stat(), 0);
        assert!(!irq.is_pending());
    }

    #[test]
    fn writing_one_does_not_set_a_bit() {
        // Software não consegue forçar uma IRQ escrevendo 1 em I_STAT.
        let mut irq = IrqController::new();
        irq.write_mask(0x07FF);
        irq.write_stat(0xFFFF);
        assert_eq!(irq.stat(), 0);
        assert!(!irq.is_pending());
    }

    #[test]
    fn mask_keeps_only_the_eleven_defined_bits() {
        let mut irq = IrqController::new();
        irq.write_mask(0xFFFF);
        assert_eq!(irq.mask(), 0x07FF);
    }
}
