//! Barramento de memória: decodifica endereços e roteia para os periféricos.
//!
//! Referência: PSX-SPX — "Memory Map", "I/O Map", "DMA Channels".
//!
//! Toda a decodificação parte do endereço **físico** ([`memory::physical`]),
//! porque KUSEG, KSEG0 e KSEG1 são espelhos da mesma memória.

use crate::bios::Bios;
use crate::cdrom::CdRom;
use crate::dma::{Direction, Dma, Port, Sync};
use crate::gpu::Gpu;
use crate::irq::{Interrupt, IrqController};
use crate::memory::{self, Ram, SCRATCHPAD_SIZE};
use crate::sio::Sio;
use crate::spu::Spu;
use crate::timers::Timers;

/// O barramento e tudo que está pendurado nele.
pub struct Bus {
    pub ram: Ram,
    pub bios: Bios,
    scratchpad: Box<[u8]>,

    pub irq: IrqController,
    pub dma: Dma,
    pub timers: Timers,
    pub gpu: Gpu,
    pub spu: Spu,
    pub cdrom: CdRom,
    pub sio: Sio,

    /// `0x1F80_1000..0x1F80_1024` — mapeamento das expansões. O BIOS escreve
    /// valores fixos aqui e nunca mais mexe.
    memory_control: [u32; 9],
    /// `0x1F80_1060` — `RAM_SIZE`.
    ram_size: u32,
    /// `0xFFFE_0130` — cache control.
    cache_control: u32,

    /// Acessos a endereços sem mapeamento, para diagnóstico.
    unhandled_reads: u64,
    unhandled_writes: u64,
    last_unhandled_address: u32,
}

impl Bus {
    pub fn new(bios: Bios) -> Self {
        Self {
            ram: Ram::new(),
            bios,
            scratchpad: vec![0; SCRATCHPAD_SIZE].into_boxed_slice(),
            irq: IrqController::new(),
            dma: Dma::new(),
            timers: Timers::new(),
            gpu: Gpu::new(),
            spu: Spu::new(),
            cdrom: CdRom::new(),
            sio: Sio::new(),
            memory_control: [0; 9],
            ram_size: 0x0000_0B88,
            cache_control: 0,
            unhandled_reads: 0,
            unhandled_writes: 0,
            last_unhandled_address: 0,
        }
    }

    pub const fn unhandled_reads(&self) -> u64 {
        self.unhandled_reads
    }

    pub const fn unhandled_writes(&self) -> u64 {
        self.unhandled_writes
    }

    pub const fn last_unhandled_address(&self) -> u32 {
        self.last_unhandled_address
    }

    fn unhandled_read(&mut self, address: u32) -> u32 {
        self.unhandled_reads += 1;
        self.last_unhandled_address = address;
        // Barramento flutuante lê como todos-uns.
        0xFFFF_FFFF
    }

    fn unhandled_write(&mut self, address: u32) {
        self.unhandled_writes += 1;
        self.last_unhandled_address = address;
    }

    // ------------------------------------------------------------- leituras

    pub fn load32(&mut self, address: u32) -> u32 {
        let physical = memory::physical(address);

        if let Some(offset) = memory::REGION_RAM.contains(physical) {
            return self.ram.read32(offset);
        }
        if let Some(offset) = memory::REGION_BIOS.contains(physical) {
            return self.bios.read32(offset);
        }
        if let Some(offset) = memory::REGION_SCRATCHPAD.contains(physical) {
            let i = offset as usize;
            return u32::from_le_bytes([
                self.scratchpad[i],
                self.scratchpad[i + 1],
                self.scratchpad[i + 2],
                self.scratchpad[i + 3],
            ]);
        }
        if let Some(offset) = memory::REGION_IO.contains(physical) {
            return self.load_io32(offset, physical);
        }
        if memory::REGION_EXPANSION_1.contains(physical).is_some()
            || memory::REGION_EXPANSION_2.contains(physical).is_some()
            || memory::REGION_EXPANSION_3.contains(physical).is_some()
        {
            // Sem cartucho de expansão: barramento flutuante.
            return 0xFFFF_FFFF;
        }
        if memory::REGION_CACHE_CONTROL.contains(physical).is_some() {
            return self.cache_control;
        }

        self.unhandled_read(address)
    }

    pub fn load16(&mut self, address: u32) -> u16 {
        let physical = memory::physical(address);

        if let Some(offset) = memory::REGION_RAM.contains(physical) {
            return self.ram.read16(offset);
        }
        if let Some(offset) = memory::REGION_BIOS.contains(physical) {
            return self.bios.read16(offset);
        }
        if let Some(offset) = memory::REGION_SCRATCHPAD.contains(physical) {
            let i = offset as usize;
            return u16::from_le_bytes([self.scratchpad[i], self.scratchpad[i + 1]]);
        }
        if let Some(offset) = memory::REGION_IO.contains(physical) {
            return self.load_io16(offset, physical);
        }
        if memory::REGION_EXPANSION_1.contains(physical).is_some()
            || memory::REGION_EXPANSION_2.contains(physical).is_some()
            || memory::REGION_EXPANSION_3.contains(physical).is_some()
        {
            return 0xFFFF;
        }

        self.unhandled_read(address) as u16
    }

    pub fn load8(&mut self, address: u32) -> u8 {
        let physical = memory::physical(address);

        if let Some(offset) = memory::REGION_RAM.contains(physical) {
            return self.ram.read8(offset);
        }
        if let Some(offset) = memory::REGION_BIOS.contains(physical) {
            return self.bios.read8(offset);
        }
        if let Some(offset) = memory::REGION_SCRATCHPAD.contains(physical) {
            return self.scratchpad[offset as usize];
        }
        if let Some(offset) = memory::REGION_IO.contains(physical) {
            return self.load_io8(offset, physical);
        }
        if memory::REGION_EXPANSION_1.contains(physical).is_some()
            || memory::REGION_EXPANSION_2.contains(physical).is_some()
            || memory::REGION_EXPANSION_3.contains(physical).is_some()
        {
            return 0xFF;
        }

        self.unhandled_read(address) as u8
    }

    // ------------------------------------------------------------- escritas

    pub fn store32(&mut self, address: u32, value: u32) {
        let physical = memory::physical(address);

        if let Some(offset) = memory::REGION_RAM.contains(physical) {
            self.ram.write32(offset, value);
            return;
        }
        if let Some(offset) = memory::REGION_SCRATCHPAD.contains(physical) {
            let i = offset as usize;
            self.scratchpad[i..i + 4].copy_from_slice(&value.to_le_bytes());
            return;
        }
        if let Some(offset) = memory::REGION_IO.contains(physical) {
            self.store_io32(offset, physical, value);
            return;
        }
        if memory::REGION_CACHE_CONTROL.contains(physical).is_some() {
            self.cache_control = value;
            return;
        }
        if memory::REGION_BIOS.contains(physical).is_some() {
            // A BIOS é ROM. Escritas são engolidas silenciosamente pelo hardware.
            return;
        }
        if memory::REGION_EXPANSION_1.contains(physical).is_some()
            || memory::REGION_EXPANSION_2.contains(physical).is_some()
            || memory::REGION_EXPANSION_3.contains(physical).is_some()
        {
            return;
        }

        self.unhandled_write(address);
    }

    pub fn store16(&mut self, address: u32, value: u16) {
        let physical = memory::physical(address);

        if let Some(offset) = memory::REGION_RAM.contains(physical) {
            self.ram.write16(offset, value);
            return;
        }
        if let Some(offset) = memory::REGION_SCRATCHPAD.contains(physical) {
            let i = offset as usize;
            self.scratchpad[i..i + 2].copy_from_slice(&value.to_le_bytes());
            return;
        }
        if let Some(offset) = memory::REGION_IO.contains(physical) {
            self.store_io16(offset, physical, value);
            return;
        }
        if memory::REGION_BIOS.contains(physical).is_some()
            || memory::REGION_EXPANSION_1.contains(physical).is_some()
            || memory::REGION_EXPANSION_2.contains(physical).is_some()
            || memory::REGION_EXPANSION_3.contains(physical).is_some()
        {
            return;
        }

        self.unhandled_write(address);
    }

    pub fn store8(&mut self, address: u32, value: u8) {
        let physical = memory::physical(address);

        if let Some(offset) = memory::REGION_RAM.contains(physical) {
            self.ram.write8(offset, value);
            return;
        }
        if let Some(offset) = memory::REGION_SCRATCHPAD.contains(physical) {
            self.scratchpad[offset as usize] = value;
            return;
        }
        if let Some(offset) = memory::REGION_IO.contains(physical) {
            self.store_io8(offset, physical, value);
            return;
        }
        if memory::REGION_BIOS.contains(physical).is_some()
            || memory::REGION_EXPANSION_1.contains(physical).is_some()
            || memory::REGION_EXPANSION_2.contains(physical).is_some()
            || memory::REGION_EXPANSION_3.contains(physical).is_some()
        {
            return;
        }

        self.unhandled_write(address);
    }

    // ------------------------------------------------------------------ I/O

    fn load_io32(&mut self, offset: u32, address: u32) -> u32 {
        match offset {
            0x000..=0x023 => self.memory_control[(offset / 4) as usize],
            0x040..=0x04F => self.sio.read(offset),
            // SIO1 não é usado por jogos; devolve valores inertes.
            0x050..=0x05F => 0,
            0x060 => self.ram_size,
            0x070 => self.irq.stat() as u32,
            0x074 => self.irq.mask() as u32,
            0x080..=0x0FF => self.dma.read(offset),
            0x100..=0x12F => self.timers.read(offset - 0x100),
            0x800..=0x803 => self.cdrom.read(offset - 0x800) as u32,
            0x810 => self.gpu.read(),
            0x814 => self.gpu.status(),
            // MDEC: o agente de vídeo ainda não existe; status "pronto e
            // ocioso" evita travar quem só consulta.
            0x820 => 0,
            0x824 => 0x8000_0000,
            0xC00..=0xFFF => {
                let low = self.spu.read(offset - 0xC00) as u32;
                let high = self.spu.read(offset - 0xC00 + 2) as u32;
                low | (high << 16)
            }
            _ => self.unhandled_read(address),
        }
    }

    fn load_io16(&mut self, offset: u32, address: u32) -> u16 {
        match offset {
            0x040..=0x04F => self.sio.read(offset) as u16,
            0x050..=0x05F => 0,
            0x070 => self.irq.stat(),
            0x074 => self.irq.mask(),
            0x100..=0x12F => self.timers.read(offset - 0x100) as u16,
            0x800..=0x803 => self.cdrom.read(offset - 0x800) as u16,
            0xC00..=0xFFF => self.spu.read(offset - 0xC00),
            _ => self.unhandled_read(address) as u16,
        }
    }

    fn load_io8(&mut self, offset: u32, address: u32) -> u8 {
        match offset {
            0x040..=0x04F => self.sio.read(offset) as u8,
            0x800..=0x803 => self.cdrom.read(offset - 0x800),
            0xC00..=0xFFF => self.spu.read(offset - 0xC00) as u8,
            _ => self.unhandled_read(address) as u8,
        }
    }

    fn store_io32(&mut self, offset: u32, address: u32, value: u32) {
        match offset {
            0x000..=0x023 => self.memory_control[(offset / 4) as usize] = value,
            0x040..=0x04F => self.sio.write(offset, value),
            0x050..=0x05F => {}
            0x060 => self.ram_size = value,
            0x070 => self.irq.write_stat(value as u16),
            0x074 => self.irq.write_mask(value as u16),
            0x080..=0x0FF => {
                if let Some(port) = self.dma.write(offset, value) {
                    self.run_dma(port);
                }
            }
            0x100..=0x12F => self.timers.write(offset - 0x100, value),
            0x800..=0x803 => self.cdrom.write(offset - 0x800, value as u8),
            0x810 => self.gpu.write_gp0(value, &mut self.irq),
            0x814 => self.gpu.write_gp1(value),
            // MDEC: comandos ainda não modelados.
            0x820 | 0x824 => {}
            0xC00..=0xFFF => {
                self.spu.write(offset - 0xC00, value as u16);
                self.spu.write(offset - 0xC00 + 2, (value >> 16) as u16);
            }
            _ => self.unhandled_write(address),
        }
    }

    fn store_io16(&mut self, offset: u32, address: u32, value: u16) {
        match offset {
            0x040..=0x04F => self.sio.write(offset, value as u32),
            0x050..=0x05F => {}
            0x070 => self.irq.write_stat(value),
            0x074 => self.irq.write_mask(value),
            0x100..=0x12F => self.timers.write(offset - 0x100, value as u32),
            0x800..=0x803 => self.cdrom.write(offset - 0x800, value as u8),
            0xC00..=0xFFF => self.spu.write(offset - 0xC00, value),
            _ => self.unhandled_write(address),
        }
    }

    fn store_io8(&mut self, offset: u32, address: u32, value: u8) {
        match offset {
            0x040..=0x04F => self.sio.write(offset, value as u32),
            0x800..=0x803 => self.cdrom.write(offset - 0x800, value),
            0xC00..=0xFFF => self.spu.write(offset - 0xC00, value as u16),
            // Expansion 2 (POST) é escrita pelo BIOS a cada etapa do boot.
            _ => self.unhandled_write(address),
        }
    }

    // ------------------------------------------------------------------ DMA

    /// Executa uma transferência de DMA até o fim.
    ///
    /// O hardware intercala DMA e CPU; aqui a transferência é atômica, o que é
    /// suficiente enquanto o scheduler não for ciclo-a-ciclo.
    pub fn run_dma(&mut self, port: Port) {
        match port {
            Port::Otc => self.run_dma_otc(),
            _ => match self.dma.channel(port).sync() {
                Sync::LinkedList => self.run_dma_linked_list(port),
                _ => self.run_dma_block(port),
            },
        }

        self.dma.channel_mut(port).finish();
        self.dma.raise_interrupt(port);
        if self.dma.take_interrupt_edge() {
            self.irq.raise(Interrupt::Dma);
        }
    }

    /// Canal 6 — limpeza da ordering table.
    ///
    /// É o único canal cabeado: o hardware ignora direção, passo e sync mode,
    /// sempre escrevendo para a RAM e sempre andando para trás. Honrar esses
    /// campos, como fazíamos, deixava o canal inerte em qualquer configuração
    /// fora da manual — o jogo pedia a tabela e recebia o lixo anterior.
    fn run_dma_otc(&mut self) {
        let channel = *self.dma.channel(Port::Otc);
        let mut address = channel.base;
        // Sem sync mode válido, o tamanho vem sempre do bloco.
        let mut remaining = match channel.transfer_size() {
            Some(size) => size,
            None => {
                let block_size = channel.block_control & 0xFFFF;
                if block_size == 0 {
                    0x1_0000
                } else {
                    block_size
                }
            }
        };

        while remaining > 0 {
            let masked = address & 0x001F_FFFC;
            // Cada entrada aponta para a anterior; a última é o marcador de fim.
            let word = if remaining == 1 {
                0x00FF_FFFF
            } else {
                masked.wrapping_sub(4) & 0x001F_FFFF
            };
            self.ram.write32(masked, word);
            address = address.wrapping_sub(4);
            remaining -= 1;
        }
    }

    fn run_dma_block(&mut self, port: Port) {
        let channel = *self.dma.channel(port);
        let step = channel.step().delta();
        let mut address = channel.base;
        let mut remaining = channel.transfer_size().unwrap_or(0);

        while remaining > 0 {
            // O hardware força o endereço para dentro dos 2 MB.
            let masked = address & 0x001F_FFFC;

            match channel.direction() {
                Direction::FromRam => {
                    let word = self.ram.read32(masked);
                    match port {
                        Port::Gpu => self.gpu.write_gp0(word, &mut self.irq),
                        Port::Spu => self.spu.dma_write(word),
                        _ => {}
                    }
                }
                Direction::ToRam => {
                    let word = match port {
                        Port::Otc => {
                            // A ordering table é construída ao contrário: cada
                            // entrada aponta para a anterior, e a última é o
                            // marcador de fim.
                            if remaining == 1 {
                                0x00FF_FFFF
                            } else {
                                masked.wrapping_sub(4) & 0x001F_FFFF
                            }
                        }
                        Port::Gpu => self.gpu.read(),
                        Port::Spu => self.spu.dma_read(),
                        Port::CdRom => self.cdrom.dma_read(),
                        _ => 0,
                    };
                    self.ram.write32(masked, word);
                }
            }

            address = address.wrapping_add(step);
            remaining -= 1;
        }
    }

    fn run_dma_linked_list(&mut self, port: Port) {
        // Só o canal da GPU usa listas encadeadas.
        if port != Port::Gpu {
            return;
        }

        let mut address = self.dma.channel(port).base & 0x001F_FFFC;

        loop {
            let header = self.ram.read32(address);
            let mut count = header >> 24;

            while count > 0 {
                address = (address + 4) & 0x001F_FFFC;
                let command = self.ram.read32(address);
                self.gpu.write_gp0(command, &mut self.irq);
                count -= 1;
            }

            // Bit 23 do header marca o fim da lista.
            if header & 0x0080_0000 != 0 {
                break;
            }
            address = header & 0x001F_FFFC;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bus() -> Bus {
        Bus::new(Bios::stub())
    }

    #[test]
    fn ram_is_reachable_through_all_three_segments() {
        let mut bus = bus();
        bus.store32(0x0000_1000, 0xCAFE_BABE);
        assert_eq!(bus.load32(0x8000_1000), 0xCAFE_BABE);
        assert_eq!(bus.load32(0xA000_1000), 0xCAFE_BABE);
    }

    #[test]
    fn bios_is_read_only() {
        let mut bus = bus();
        bus.store32(0xBFC0_0000, 0xDEAD_BEEF);
        assert_eq!(bus.load32(0xBFC0_0000), 0, "escrita na ROM ignorada");
        assert_eq!(
            bus.unhandled_writes(),
            0,
            "e não conta como acesso inválido"
        );
    }

    #[test]
    fn scratchpad_round_trips() {
        let mut bus = bus();
        bus.store32(0x1F80_0000, 0x1122_3344);
        assert_eq!(bus.load32(0x1F80_0000), 0x1122_3344);
        assert_eq!(bus.load8(0x1F80_0000), 0x44);
        bus.store8(0x1F80_0010, 0xAB);
        assert_eq!(bus.load8(0x1F80_0010), 0xAB);
    }

    #[test]
    fn irq_registers_are_wired() {
        let mut bus = bus();
        bus.store16(0x1F80_1074, 0x0001);
        assert_eq!(bus.load16(0x1F80_1074), 0x0001);
        bus.irq.raise(Interrupt::VBlank);
        assert_eq!(bus.load16(0x1F80_1070), 0x0001);
        // Acknowledge.
        bus.store16(0x1F80_1070, 0x0000);
        assert_eq!(bus.load16(0x1F80_1070), 0x0000);
    }

    #[test]
    fn missing_expansion_reads_as_all_ones() {
        let mut bus = bus();
        assert_eq!(bus.load8(0x1F00_0000), 0xFF);
        assert_eq!(bus.load32(0x1F80_2000), 0xFFFF_FFFF);
        assert_eq!(bus.unhandled_reads(), 0);
    }

    #[test]
    fn cache_control_lives_in_kseg2() {
        let mut bus = bus();
        bus.store32(0xFFFE_0130, 0x0001_E988);
        assert_eq!(bus.load32(0xFFFE_0130), 0x0001_E988);
    }

    #[test]
    fn gpustat_reports_ready_for_commands() {
        let mut bus = bus();
        let status = bus.load32(0x1F80_1814);
        assert_ne!(status & (1 << 26), 0, "pronto para receber comando");
        assert_ne!(status & (1 << 28), 0, "pronto para bloco de DMA");
    }

    #[test]
    fn otc_dma_builds_a_reverse_ordering_table() {
        let mut bus = bus();

        // Habilita o canal 6 em DPCR e configura uma tabela de 4 entradas
        // terminando em 0x1000.
        bus.store32(0x1F80_10F0, 1 << 27);
        // MADR: endereço base.
        bus.store32(0x1F80_10E0, 0x0000_1000);
        // BCR: 4 palavras.
        bus.store32(0x1F80_10E4, 4);
        // CHCR: ToRam, passo para trás, manual, enable + trigger.
        bus.store32(0x1F80_10E8, (1 << 24) | (1 << 28) | 2);

        assert_eq!(bus.ram.read32(0x1000), 0x0FFC);
        assert_eq!(bus.ram.read32(0x0FFC), 0x0FF8);
        assert_eq!(bus.ram.read32(0x0FF8), 0x0FF4);
        assert_eq!(bus.ram.read32(0x0FF4), 0x00FF_FFFF, "marcador de fim");
    }

    #[test]
    fn dma_completion_raises_the_dma_interrupt() {
        let mut bus = bus();
        bus.irq.write_mask(0x07FF);
        // Habilita a IRQ do canal 6 em DICR.
        bus.store32(0x1F80_10F4, (1 << 23) | (1 << 22));
        bus.store32(0x1F80_10F0, 1 << 27);
        bus.store32(0x1F80_10E0, 0x0000_1000);
        bus.store32(0x1F80_10E4, 2);
        bus.store32(0x1F80_10E8, (1 << 24) | (1 << 28) | 2);

        assert_ne!(bus.irq.stat() & (1 << Interrupt::Dma as u16), 0);
    }

    #[test]
    fn channel_clears_its_busy_bit_after_running() {
        let mut bus = bus();
        bus.store32(0x1F80_10F0, 1 << 27);
        bus.store32(0x1F80_10E0, 0x0000_1000);
        bus.store32(0x1F80_10E4, 2);
        bus.store32(0x1F80_10E8, (1 << 24) | (1 << 28) | 2);

        let chcr = bus.load32(0x1F80_10E8);
        assert_eq!(chcr & (1 << 24), 0, "bit de busy limpo");
        assert_eq!(chcr & (1 << 28), 0, "bit de trigger limpo");
    }

    #[test]
    fn gpu_linked_list_dma_feeds_gp0() {
        let mut bus = bus();

        // Lista com um nó: header (1 palavra de payload, fim de lista) seguido
        // de um GP0(0xE3) que define o canto superior esquerdo da área.
        bus.ram.write32(0x1000, 0x0100_0000 | 0x0080_0000);
        bus.ram.write32(0x1004, 0xE300_0000 | (20 << 10) | 10);

        bus.store32(0x1F80_10F0, 1 << 11); // habilita canal 2 em DPCR
        bus.store32(0x1F80_10A0, 0x0000_1000); // MADR do canal 2
        bus.store32(0x1F80_10A4, 0); // BCR
                                     // CHCR: FromRam, linked list, enable.
        bus.store32(0x1F80_10A8, (1 << 24) | (2 << 9) | 1);

        // GP1(0x10) com payload 3 devolve a área de desenho pelo GPUREAD.
        bus.store32(0x1F80_1814, 0x1000_0003);
        let area = bus.load32(0x1F80_1810);
        assert_eq!(area & 0x3FF, 10, "left");
        assert_eq!((area >> 10) & 0x1FF, 20, "top");
    }

    #[test]
    fn unmapped_io_is_counted_instead_of_panicking() {
        let mut bus = bus();
        // 0x1F80_1830 fica entre a GPU e o MDEC: sem dono.
        assert_eq!(bus.load32(0x1F80_1830), 0xFFFF_FFFF);
        assert_eq!(bus.unhandled_reads(), 1);
        assert_eq!(bus.last_unhandled_address(), 0x1F80_1830);

        bus.store32(0x1F80_1830, 0);
        assert_eq!(bus.unhandled_writes(), 1);
    }

    #[test]
    fn spu_block_is_mapped_up_to_the_end_of_the_io_region() {
        let mut bus = bus();
        bus.store16(0x1F80_1DAA, 0x0025); // SPUCNT
        assert_eq!(bus.load16(0x1F80_1DAE) & 0x3F, 0x25, "SPUSTAT espelha");
        assert_eq!(bus.unhandled_reads(), 0);
    }

    #[test]
    fn timers_are_wired_through_the_bus() {
        let mut bus = bus();
        bus.store32(0x1F80_1108, 1234); // target do timer 0
        assert_eq!(bus.load32(0x1F80_1108), 1234);
        bus.timers.step(10, &mut bus.irq);
        assert_eq!(bus.load32(0x1F80_1100), 10);
    }

    #[test]
    fn cdrom_index_register_is_reachable_byte_wise() {
        let mut bus = bus();
        bus.store8(0x1F80_1800, 1);
        assert_eq!(bus.load8(0x1F80_1800) & 3, 1);
    }
}
