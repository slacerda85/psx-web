//! Controlador de DMA.
//!
//! Referência: PSX-SPX — "DMA Channels".
//!
//! Sete canais compartilham o barramento com a CPU. Cada canal tem três
//! registradores (`MADR`, `BCR`, `CHCR`) em `0x1F80_1080 + n*0x10`, e o bloco
//! termina com `DPCR` (`0x1F80_10F0`) e `DICR` (`0x1F80_10F4`).
//!
//! O movimento de dados em si vive em [`crate::bus`], porque precisa alcançar
//! a RAM e os periféricos ao mesmo tempo.

/// Os sete canais de DMA, na ordem do hardware.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
pub enum Port {
    /// Canal 0 — MDEC, entrada de dados comprimidos.
    MdecIn = 0,
    /// Canal 1 — MDEC, saída de imagem descomprimida.
    MdecOut = 1,
    /// Canal 2 — GPU (listas de comandos e transferências de VRAM).
    Gpu = 2,
    /// Canal 3 — CD-ROM.
    CdRom = 3,
    /// Canal 4 — SPU.
    Spu = 4,
    /// Canal 5 — Expansion Port.
    Pio = 5,
    /// Canal 6 — OTC, limpa a ordering table.
    Otc = 6,
}

impl Port {
    pub const ALL: [Port; 7] = [
        Port::MdecIn,
        Port::MdecOut,
        Port::Gpu,
        Port::CdRom,
        Port::Spu,
        Port::Pio,
        Port::Otc,
    ];

    pub const fn from_index(index: usize) -> Option<Port> {
        match index {
            0 => Some(Port::MdecIn),
            1 => Some(Port::MdecOut),
            2 => Some(Port::Gpu),
            3 => Some(Port::CdRom),
            4 => Some(Port::Spu),
            5 => Some(Port::Pio),
            6 => Some(Port::Otc),
            _ => None,
        }
    }
}

/// Sentido da transferência.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Periférico → RAM.
    ToRam,
    /// RAM → periférico.
    FromRam,
}

/// Como o endereço avança a cada palavra.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Forward,
    Backward,
}

impl Step {
    pub const fn delta(self) -> u32 {
        match self {
            Step::Forward => 4,
            // Complemento de dois: somar isso equivale a subtrair 4.
            Step::Backward => 0xFFFF_FFFC,
        }
    }
}

/// Modo de sincronização do canal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sync {
    /// Bloco único, disparado manualmente (`CHCR.28`).
    Manual,
    /// Blocos sincronizados com o "data request" do periférico.
    Request,
    /// Lista encadeada (usada pela GPU).
    LinkedList,
}

/// Registradores de um canal.
#[derive(Debug, Clone, Copy, Default)]
pub struct Channel {
    /// `MADR` — endereço base na RAM.
    pub base: u32,
    /// `BCR` — tamanho do bloco / número de blocos.
    pub block_control: u32,
    /// `CHCR` — controle.
    pub control: u32,
}

impl Channel {
    pub const fn direction(&self) -> Direction {
        if self.control & 1 != 0 {
            Direction::FromRam
        } else {
            Direction::ToRam
        }
    }

    pub const fn step(&self) -> Step {
        if self.control & 2 != 0 {
            Step::Backward
        } else {
            Step::Forward
        }
    }

    pub const fn sync(&self) -> Sync {
        match (self.control >> 9) & 3 {
            0 => Sync::Manual,
            1 => Sync::Request,
            _ => Sync::LinkedList,
        }
    }

    /// `true` quando o canal está pronto para transferir.
    ///
    /// No modo manual o software precisa ligar tanto o bit de "enable" (24)
    /// quanto o de "trigger" (28).
    pub const fn is_active(&self) -> bool {
        let enabled = self.control & (1 << 24) != 0;
        match self.sync() {
            Sync::Manual => enabled && self.control & (1 << 28) != 0,
            _ => enabled,
        }
    }

    /// Número de palavras a transferir, quando conhecido de antemão.
    pub const fn transfer_size(&self) -> Option<u32> {
        let block_size = self.block_control & 0xFFFF;
        // Tamanho 0 significa 0x10000 palavras.
        let block_size = if block_size == 0 {
            0x1_0000
        } else {
            block_size
        };
        match self.sync() {
            Sync::Manual => Some(block_size),
            Sync::Request => {
                let block_count = (self.block_control >> 16) & 0xFFFF;
                Some(block_size * block_count)
            }
            // A lista encadeada só termina quando encontra o marcador de fim.
            Sync::LinkedList => None,
        }
    }

    /// Marca a transferência como concluída, limpando os bits de disparo.
    pub fn finish(&mut self) {
        self.control &= !(1 << 24);
        self.control &= !(1 << 28);
    }

    /// Máscara de bits graváveis de `CHCR`.
    pub fn write_control(&mut self, value: u32) {
        self.control = value & 0x7177_0703;
    }
}

/// Estado do bloco `DICR`.
#[derive(Debug, Clone, Copy, Default)]
pub struct DmaInterrupt {
    /// Bits 0..5 — sem função conhecida, mas graváveis e legíveis.
    unknown: u32,
    /// Bit 15 — força a IRQ independentemente das máscaras.
    force: bool,
    /// Bits 16..22 — habilitação por canal.
    enable: u32,
    /// Bit 23 — habilitação mestre.
    master_enable: bool,
    /// Bits 24..30 — flags por canal.
    flags: u32,
}

impl DmaInterrupt {
    /// Bit 31, derivado: `force || (master_enable && (enable & flags) != 0)`.
    pub const fn master_flag(&self) -> bool {
        self.force || (self.master_enable && (self.enable & self.flags) != 0)
    }

    pub const fn to_u32(self) -> u32 {
        (self.unknown & 0x3F)
            | ((self.force as u32) << 15)
            | ((self.enable & 0x7F) << 16)
            | ((self.master_enable as u32) << 23)
            | ((self.flags & 0x7F) << 24)
            | ((self.master_flag() as u32) << 31)
    }

    pub fn write(&mut self, value: u32) {
        self.unknown = value & 0x3F;
        self.force = value & (1 << 15) != 0;
        self.enable = (value >> 16) & 0x7F;
        self.master_enable = value & (1 << 23) != 0;
        // Escrever 1 num flag o limpa (acknowledge).
        self.flags &= !((value >> 24) & 0x7F);
    }

    /// Sinaliza o fim de uma transferência no canal `port`.
    pub fn raise(&mut self, port: Port) {
        self.flags |= 1 << (port as u32);
    }
}

/// O controlador de DMA.
#[derive(Debug, Clone)]
pub struct Dma {
    /// `DPCR` — prioridade e habilitação por canal.
    control: u32,
    interrupt: DmaInterrupt,
    channels: [Channel; 7],
    /// `true` na borda de subida do bit 31 de `DICR`, para gerar a IRQ.
    previous_master_flag: bool,
}

impl Dma {
    pub fn new() -> Self {
        Self {
            // Valor pós-reset documentado no PSX-SPX.
            control: 0x0765_4321,
            interrupt: DmaInterrupt::default(),
            channels: [Channel::default(); 7],
            previous_master_flag: false,
        }
    }

    pub fn channel(&self, port: Port) -> &Channel {
        &self.channels[port as usize]
    }

    pub fn channel_mut(&mut self, port: Port) -> &mut Channel {
        &mut self.channels[port as usize]
    }

    pub const fn control(&self) -> u32 {
        self.control
    }

    pub const fn interrupt(&self) -> DmaInterrupt {
        self.interrupt
    }

    /// Um canal só roda se estiver ativo **e** habilitado em `DPCR`.
    pub fn is_enabled(&self, port: Port) -> bool {
        self.control & (1 << (port as u32 * 4 + 3)) != 0
    }

    pub fn raise_interrupt(&mut self, port: Port) {
        self.interrupt.raise(port);
    }

    /// Consome a borda de subida do flag mestre de IRQ, se houver.
    ///
    /// A interrupção de DMA em `I_STAT` é disparada pela *transição* de 0 para
    /// 1 do bit 31 de `DICR`, não pelo nível.
    pub fn take_interrupt_edge(&mut self) -> bool {
        let current = self.interrupt.master_flag();
        let edge = current && !self.previous_master_flag;
        self.previous_master_flag = current;
        edge
    }

    /// Leitura de um registrador do bloco de DMA (offset dentro de `0x1F80_1080`).
    pub fn read(&self, offset: u32) -> u32 {
        let register = offset & 0x7F;
        match register {
            0x70 => self.control,
            0x74 => self.interrupt.to_u32(),
            _ => {
                let index = (register >> 4) as usize;
                if index >= 7 {
                    return 0;
                }
                match register & 0x0F {
                    0x00 => self.channels[index].base,
                    0x04 => self.channels[index].block_control,
                    0x08 => self.channels[index].control,
                    _ => 0,
                }
            }
        }
    }

    /// Escrita num registrador do bloco de DMA. Devolve o canal que deve rodar.
    #[must_use = "o canal devolvido precisa ser executado pelo bus"]
    pub fn write(&mut self, offset: u32, value: u32) -> Option<Port> {
        let register = offset & 0x7F;
        match register {
            0x70 => {
                self.control = value;
                None
            }
            0x74 => {
                self.interrupt.write(value);
                None
            }
            _ => {
                let index = (register >> 4) as usize;
                let port = Port::from_index(index)?;
                match register & 0x0F {
                    // MADR guarda apenas endereços de RAM alinhados.
                    0x00 => self.channels[index].base = value & 0x00FF_FFFC,
                    0x04 => self.channels[index].block_control = value,
                    0x08 => self.channels[index].write_control(value),
                    _ => {}
                }
                if self.channels[index].is_active() && self.is_enabled(port) {
                    Some(port)
                } else {
                    None
                }
            }
        }
    }
}

impl Default for Dma {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_channel_needs_both_enable_and_trigger() {
        let mut channel = Channel::default();
        channel.write_control(1 << 24);
        assert!(!channel.is_active(), "só enable não basta no modo manual");
        channel.write_control((1 << 24) | (1 << 28));
        assert!(channel.is_active());
    }

    #[test]
    fn request_channel_only_needs_enable() {
        let mut channel = Channel::default();
        channel.write_control((1 << 24) | (1 << 9));
        assert_eq!(channel.sync(), Sync::Request);
        assert!(channel.is_active());
    }

    #[test]
    fn block_size_zero_means_65536_words() {
        let mut channel = Channel::default();
        channel.write_control(1 << 24);
        channel.block_control = 0;
        assert_eq!(channel.transfer_size(), Some(0x1_0000));
    }

    #[test]
    fn request_mode_multiplies_block_size_by_count() {
        let mut channel = Channel::default();
        channel.write_control((1 << 24) | (1 << 9));
        channel.block_control = (4 << 16) | 16;
        assert_eq!(channel.transfer_size(), Some(64));
    }

    #[test]
    fn linked_list_size_is_unknown_upfront() {
        let mut channel = Channel::default();
        channel.write_control((1 << 24) | (2 << 9));
        assert_eq!(channel.sync(), Sync::LinkedList);
        assert_eq!(channel.transfer_size(), None);
    }

    #[test]
    fn backward_step_subtracts_four() {
        let mut channel = Channel::default();
        channel.write_control(2);
        assert_eq!(channel.step(), Step::Backward);
        assert_eq!(0x1000u32.wrapping_add(channel.step().delta()), 0x0FFC);
    }

    #[test]
    fn madr_keeps_only_word_aligned_ram_addresses() {
        let mut dma = Dma::new();
        let _ = dma.write(0x20, 0xFFFF_FFFF); // MADR do canal 2
        assert_eq!(dma.channel(Port::Gpu).base, 0x00FF_FFFC);
    }

    #[test]
    fn dicr_master_flag_is_derived() {
        let mut interrupt = DmaInterrupt::default();
        interrupt.write((1 << 23) | (0x7F << 16)); // master + todos habilitados
        assert!(!interrupt.master_flag());

        interrupt.raise(Port::Gpu);
        assert!(interrupt.master_flag());
        assert_ne!(interrupt.to_u32() & 0x8000_0000, 0);
    }

    #[test]
    fn writing_one_acknowledges_a_dicr_flag() {
        let mut interrupt = DmaInterrupt::default();
        interrupt.write((1 << 23) | (0x7F << 16));
        interrupt.raise(Port::Gpu);
        assert_ne!(interrupt.to_u32() & (1 << 26), 0);

        // Acknowledge: escreve 1 no bit 26 (flag do canal 2).
        interrupt.write((1 << 23) | (0x7F << 16) | (1 << 26));
        assert_eq!(interrupt.to_u32() & (1 << 26), 0);
        assert!(!interrupt.master_flag());
    }

    #[test]
    fn force_irq_bypasses_the_masks() {
        let mut interrupt = DmaInterrupt::default();
        interrupt.write(1 << 15);
        assert!(interrupt.master_flag(), "bit 15 força a IRQ");
    }

    #[test]
    fn interrupt_edge_fires_only_once() {
        let mut dma = Dma::new();
        let _ = dma.write(0x74, (1 << 23) | (0x7F << 16));
        dma.raise_interrupt(Port::Otc);

        assert!(dma.take_interrupt_edge(), "borda de subida");
        assert!(!dma.take_interrupt_edge(), "nível alto não dispara de novo");
    }

    #[test]
    fn channel_is_only_started_when_enabled_in_dpcr() {
        let mut dma = Dma::new();
        // DPCR pós-reset habilita o canal 6 (bit 27)?  0x07654321 → nibble 6 = 0.
        let _ = dma.write(0x70, 0);
        let started = dma.write(0x68, (1 << 24) | (1 << 28));
        assert_eq!(started, None, "canal desabilitado em DPCR não roda");

        let _ = dma.write(0x70, 1 << 27);
        let started = dma.write(0x68, (1 << 24) | (1 << 28));
        assert_eq!(started, Some(Port::Otc));
    }

    #[test]
    fn finish_clears_the_busy_bits() {
        let mut channel = Channel::default();
        channel.write_control((1 << 24) | (1 << 28));
        channel.finish();
        assert!(!channel.is_active());
        assert_eq!(channel.control & (1 << 24), 0);
        assert_eq!(channel.control & (1 << 28), 0);
    }
}
