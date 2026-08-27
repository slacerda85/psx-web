//! O console completo: junta CPU, barramento e periféricos num loop de frame.
//!
//! Esta é a **API pública** que o embedder consome. `psx-wasm` é uma casca
//! fina em cima daqui, e os testes de integração usam exatamente o mesmo
//! caminho que o navegador.

use crate::bios::Bios;
use crate::bus::Bus;
use crate::cdrom::Disc;
use crate::cpu::Cpu;
use crate::exe::Executable;
use crate::gpu::{FRAME_HEIGHT_MAX, FRAME_WIDTH_MAX};
use crate::irq::Interrupt;
use crate::memory;
use crate::sio::ButtonState;
use crate::{PsxError, Region};

/// Largura máxima do framebuffer entregue ao frontend.
pub const VIDEO_WIDTH_MAX: usize = FRAME_WIDTH_MAX;
/// Altura máxima do framebuffer entregue ao frontend.
pub const VIDEO_HEIGHT_MAX: usize = FRAME_HEIGHT_MAX;

/// Endereço em que o BIOS entrega o controle ao shell / ao jogo.
///
/// PSX-SPX — "BIOS Memory Map": é aqui que um `.exe` sideloadado deve ser
/// injetado, com o kernel já inicializado.
pub const SHELL_ENTRY_POINT: u32 = 0x8003_0000;

/// Resumo do que aconteceu num frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FrameStats {
    /// Ciclos de CPU executados.
    pub cycles: u64,
    /// Instruções executadas (aproximado: uma por `step`).
    pub instructions: u64,
}

/// O console.
pub struct System {
    cpu: Cpu,
    bus: Bus,
    region: Region,
    /// Sobra de ciclos de uma scanline para a próxima.
    cycle_debt: i64,
    /// Texto emitido pelo programa via `putchar` do kernel.
    tty: String,
}

impl System {
    /// Cria um console com a BIOS fornecida pelo usuário, região NTSC.
    pub fn new(bios: Bios) -> Self {
        Self::with_region(bios, Region::Ntsc)
    }

    pub fn with_region(bios: Bios, region: Region) -> Self {
        Self {
            cpu: Cpu::new(),
            bus: Bus::new(bios),
            region,
            cycle_debt: 0,
            tty: String::new(),
        }
    }

    /// Reinicia o console mantendo a BIOS e o disco carregados.
    ///
    /// Apertar reset no console não abre a bandeja — o disco continua lá.
    pub fn reset(&mut self) {
        self.cpu.reset();
        let bios = self.bus.bios.clone();
        let disc = self.bus.cdrom.disc().cloned();
        self.bus = Bus::new(bios);
        if let Some(disc) = disc {
            self.bus.cdrom.insert_disc(disc);
        }
        self.cycle_debt = 0;
        self.tty.clear();
    }

    pub const fn region(&self) -> Region {
        self.region
    }

    pub fn set_region(&mut self, region: Region) {
        self.region = region;
    }

    pub fn cpu(&self) -> &Cpu {
        &self.cpu
    }

    pub fn bus(&self) -> &Bus {
        &self.bus
    }

    pub fn bus_mut(&mut self) -> &mut Bus {
        &mut self.bus
    }

    /// Executa um frame inteiro de vídeo.
    pub fn run_frame(&mut self) -> FrameStats {
        let scanlines = self.region.scanlines_per_frame();
        let cycles_per_frame = self.region.cycles_per_frame() as i64;
        let cycles_per_scanline = cycles_per_frame / scanlines as i64;
        // Linha em que começa o retraço vertical (fim da área visível).
        let vblank_line = match self.region {
            Region::Ntsc => 240,
            Region::Pal => 256,
        };

        let mut stats = FrameStats::default();

        for line in 0..scanlines {
            if line == vblank_line {
                self.bus.irq.raise(Interrupt::VBlank);
            }
            let (cycles, instructions) = self.run_scanline(cycles_per_scanline);
            stats.cycles += cycles;
            stats.instructions += instructions;
        }

        self.bus.gpu.end_of_frame();
        stats
    }

    fn run_scanline(&mut self, budget: i64) -> (u64, u64) {
        self.cycle_debt += budget;

        let mut spent = 0u64;
        let mut instructions = 0u64;

        while self.cycle_debt > 0 {
            self.capture_tty();
            let cycles = self.cpu.step(&mut self.bus);
            self.cycle_debt -= cycles as i64;
            spent += cycles as u64;
            instructions += 1;

            // SIO0 e timers andam junto com a CPU, e não em bloco no fim da
            // scanline. O `/ACK` do controller chega ~338 ciclos depois do
            // byte e o BIOS desiste antes disso; e um jogo que lê o contador no
            // meio do frame veria saltos de 2145 em 2145 no lugar do valor
            // corrente. Custa ~8% de desempenho e é o que faz o contador bater
            // exatamente com o console.
            self.bus.sio.step(cycles, &mut self.bus.irq);
            self.bus.timers.step(cycles, &mut self.bus.irq);
        }

        // Os periféricos avançam em bloco no fim da scanline. Isso é grosso o
        // bastante para o boot e para jogos que não dependem de timing de
        // sub-scanline; refiná-lo é trabalho do scheduler ciclo-a-ciclo.
        let elapsed = spent as u32;
        self.bus.cdrom.step(elapsed, &mut self.bus.irq);
        self.bus.spu.step(elapsed);

        (spent, instructions)
    }

    /// Executa até o BIOS alcançar o ponto de entrada do shell, ou até gastar
    /// `max_cycles`. Devolve `true` se chegou lá.
    ///
    /// É o gancho para sideload de `.exe`: o kernel já está inicializado.
    pub fn run_until_shell(&mut self, max_cycles: u64) -> bool {
        let mut spent = 0u64;
        let mut since_peripherals = 0u32;

        while spent < max_cycles {
            if self.cpu.pc() == SHELL_ENTRY_POINT {
                return true;
            }
            let cycles = self.cpu.step(&mut self.bus);
            spent += cycles as u64;
            since_peripherals += cycles;

            // Os periféricos precisam andar, senão o BIOS trava esperando
            // VBlank ou o CD-ROM.
            if since_peripherals >= 2000 {
                self.bus.timers.step(since_peripherals, &mut self.bus.irq);
                self.bus.cdrom.step(since_peripherals, &mut self.bus.irq);
                self.bus.spu.step(since_peripherals);
                self.bus.irq.raise(Interrupt::VBlank);
                since_peripherals = 0;
            }
        }
        false
    }

    /// Intercepta as chamadas de `putchar` do kernel para capturar a TTY.
    ///
    /// O BIOS expõe as funções por três tabelas — `0xA0`, `0xB0` e `0xC0` —
    /// entrando sempre pelo mesmo endereço, com o número da função em `$t1`.
    /// Um console retail manda essa saída para a porta serial, que num PSX sem
    /// expansão não vai a lugar nenhum; interceptá-la aqui é o que torna
    /// legível o resultado de uma suíte de testes de hardware, que reporta
    /// tudo por texto.
    fn capture_tty(&mut self) {
        let function = self.cpu.reg(9) & 0xFF;
        let is_putchar = match self.cpu.pc() {
            0xA0 => function == 0x3C,
            0xB0 => function == 0x3D,
            _ => return,
        };
        if !is_putchar {
            return;
        }
        let byte = self.cpu.reg(4) as u8;
        // Ignora o NUL final que algumas rotinas emitem.
        if byte != 0 {
            self.tty.push(byte as char);
        }
    }

    /// Texto emitido pelo programa via `putchar` desde a última coleta.
    pub fn take_tty(&mut self) -> String {
        std::mem::take(&mut self.tty)
    }

    /// Texto acumulado, sem esvaziar o buffer.
    pub fn tty(&self) -> &str {
        &self.tty
    }

    /// Insere uma imagem de disco (ISO ou BIN de faixa única).
    ///
    /// O formato é deduzido do conteúdo, não da extensão. Para um jogo com
    /// folha CUE, use [`Self::load_disc_with_cue`]: sem ela as faixas de
    /// áudio ficam invisíveis.
    pub fn load_disc(&mut self, image: Vec<u8>) -> Result<(), PsxError> {
        let disc = Disc::from_image(image)?;
        self.bus.cdrom.insert_disc(disc);
        Ok(())
    }

    /// Insere uma imagem descrita por uma folha CUE.
    ///
    /// O core não abre arquivos: quem localiza o binário que a folha
    /// referencia é o embedder, que é quem tem sistema de arquivos.
    pub fn load_disc_with_cue(&mut self, cue: &str, image: Vec<u8>) -> Result<(), PsxError> {
        let disc = Disc::from_cue(cue, image)?;
        self.bus.cdrom.insert_disc(disc);
        Ok(())
    }

    /// Abre a bandeja.
    pub fn eject_disc(&mut self) {
        self.bus.cdrom.eject();
    }

    /// A imagem inserida, se houver.
    pub fn disc(&self) -> Option<&Disc> {
        self.bus.cdrom.disc()
    }

    /// Carrega um `PS-X EXE` em RAM e salta para ele.
    ///
    /// Chame [`Self::run_until_shell`] antes, para que o kernel esteja pronto.
    pub fn load_exe(&mut self, data: &[u8]) -> Result<(), PsxError> {
        let executable = Executable::parse(data)?;
        let header = executable.header;

        // A região de memfill é zerada antes da carga.
        if header.memfill_size > 0 {
            let start = memory::physical(header.memfill_start);
            for offset in 0..header.memfill_size {
                self.bus.ram.write8(start.wrapping_add(offset), 0);
            }
        }

        let destination = memory::physical(header.destination);
        for (index, byte) in executable.body.iter().enumerate() {
            self.bus.ram.write8(destination + index as u32, *byte);
        }

        self.cpu.set_pc(header.initial_pc);
        self.cpu.set_reg_direct(28, header.initial_gp);
        if let Some(sp) = header.initial_sp() {
            self.cpu.set_reg_direct(29, sp);
            self.cpu.set_reg_direct(30, sp);
        }

        Ok(())
    }

    // ------------------------------------------------------------- entrada

    /// Atualiza os botões de um slot (0 ou 1).
    pub fn set_buttons(&mut self, slot: usize, state: ButtonState) {
        self.bus.sio.set_buttons(slot, state);
    }

    // -------------------------------------------------------------- saída

    /// Framebuffer RGBA8 do último frame renderizado.
    pub fn framebuffer(&self) -> &[u8] {
        self.bus.gpu.framebuffer()
    }

    pub fn frame_width(&self) -> u32 {
        self.bus.gpu.frame_width()
    }

    pub fn frame_height(&self) -> u32 {
        self.bus.gpu.frame_height()
    }

    /// Retira amostras de áudio produzidas desde a última chamada.
    pub fn drain_audio(&mut self, out: &mut [i16]) -> usize {
        self.bus.spu.drain_samples(out)
    }

    /// Contadores de coisas ainda não implementadas, para a UI de diagnóstico.
    pub fn diagnostics(&self) -> Diagnostics {
        Diagnostics {
            gte_unimplemented: self.cpu.gte.unimplemented_commands(),
            gpu_unhandled: self.bus.gpu.unhandled_commands(),
            cdrom_unimplemented: self.bus.cdrom.unimplemented_commands(),
            bus_unhandled_reads: self.bus.unhandled_reads(),
            bus_unhandled_writes: self.bus.unhandled_writes(),
        }
    }
}

/// Quantas operações caíram em código ainda não implementado.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Diagnostics {
    pub gte_unimplemented: u64,
    pub gpu_unhandled: u64,
    pub cdrom_unimplemented: u64,
    pub bus_unhandled_reads: u64,
    pub bus_unhandled_writes: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Console com um programa em RAM em vez de uma BIOS real.
    fn system_running(program: &[u32]) -> System {
        let mut system = System::new(Bios::stub());
        for (index, word) in program.iter().enumerate() {
            system.bus.ram.write32(index as u32 * 4, *word);
        }
        system.cpu.set_pc(0x0000_0000);
        system
    }

    #[test]
    fn ntsc_frame_is_about_564480_cycles() {
        assert_eq!(Region::Ntsc.cycles_per_frame(), 564_480);
        assert_eq!(Region::Pal.cycles_per_frame(), 677_376);
    }

    #[test]
    fn run_frame_executes_roughly_a_frame_worth_of_cycles() {
        // Loop infinito: `beq $zero, $zero, -1` + nop.
        let mut system = system_running(&[0x1000_FFFF, 0x0000_0000]);
        let stats = system.run_frame();

        let expected = Region::Ntsc.cycles_per_frame() as u64;
        let slack = expected / 100;
        assert!(
            stats.cycles >= expected - slack && stats.cycles <= expected + slack,
            "esperado ~{expected}, obtido {}",
            stats.cycles
        );
        assert!(stats.instructions > 0);
    }

    #[test]
    fn run_frame_raises_vblank() {
        let mut system = system_running(&[0x1000_FFFF, 0x0000_0000]);
        system.run_frame();
        assert_ne!(
            system.bus.irq.stat() & (1 << Interrupt::VBlank as u16),
            0,
            "VBlank sinalizado ao fim da área visível"
        );
    }

    #[test]
    fn frame_produces_a_framebuffer_of_the_configured_size() {
        let mut system = system_running(&[0x1000_FFFF, 0x0000_0000]);
        system.run_frame();
        // 320×240 é o default pós-reset.
        assert_eq!(system.frame_width(), 320);
        assert_eq!(system.frame_height(), 240);
        assert!(system.framebuffer().len() >= 320 * 240 * 4);
    }

    #[test]
    fn display_shows_what_the_gpu_drew() {
        let mut system = System::new(Bios::stub());
        // Habilita o display e desenha um retângulo vermelho na origem.
        system.bus.gpu.write_gp1(0x0300_0000); // display enable
        system
            .bus
            .gpu
            .write_gp0(0xE400_0000 | (255 << 10) | 640, &mut system.bus.irq);
        system.bus.gpu.write_gp0(0x0200_00FF, &mut system.bus.irq); // fill: vermelho
        system.bus.gpu.write_gp0(0x0000_0000, &mut system.bus.irq); // em (0,0)
        system.bus.gpu.write_gp0(0x0010_0010, &mut system.bus.irq); // 16×16

        system.bus.gpu.end_of_frame();

        let pixel = &system.framebuffer()[0..4];
        assert_eq!(pixel, &[0xFF, 0, 0, 0xFF], "vermelho no canto superior");
    }

    #[test]
    fn audio_accumulates_over_a_frame() {
        let mut system = system_running(&[0x1000_FFFF, 0x0000_0000]);
        system.run_frame();

        let mut buffer = vec![0i16; 4096];
        let written = system.drain_audio(&mut buffer);
        // 44100 / 60 = 735 frames estéreo = 1470 valores.
        assert!(
            (1400..1600).contains(&written),
            "esperado ~1470 amostras, obtido {written}"
        );
    }

    #[test]
    fn loading_an_exe_sets_pc_gp_and_sp() {
        let mut data = vec![0u8; crate::exe::HEADER_SIZE];
        data[0..8].copy_from_slice(b"PS-X EXE");
        data[0x10..0x14].copy_from_slice(&0x8001_0000u32.to_le_bytes());
        data[0x14..0x18].copy_from_slice(&0x8002_0000u32.to_le_bytes());
        data[0x18..0x1C].copy_from_slice(&0x8001_0000u32.to_le_bytes());
        data[0x1C..0x20].copy_from_slice(&4u32.to_le_bytes());
        data[0x30..0x34].copy_from_slice(&0x801F_FF00u32.to_le_bytes());
        // Corpo: um `nop`.
        data.extend_from_slice(&0u32.to_le_bytes());

        let mut system = System::new(Bios::stub());
        system.load_exe(&data).unwrap();

        assert_eq!(system.cpu.pc(), 0x8001_0000);
        assert_eq!(system.cpu.reg(28), 0x8002_0000);
        assert_eq!(system.cpu.reg(29), 0x801F_FF00);
        assert_eq!(system.cpu.reg(30), 0x801F_FF00);
    }

    #[test]
    fn memfill_region_is_zeroed_before_loading() {
        let mut data = vec![0u8; crate::exe::HEADER_SIZE];
        data[0..8].copy_from_slice(b"PS-X EXE");
        data[0x10..0x14].copy_from_slice(&0x8001_0000u32.to_le_bytes());
        data[0x18..0x1C].copy_from_slice(&0x8001_0000u32.to_le_bytes());
        data[0x1C..0x20].copy_from_slice(&4u32.to_le_bytes());
        data[0x28..0x2C].copy_from_slice(&0x8005_0000u32.to_le_bytes());
        data[0x2C..0x30].copy_from_slice(&16u32.to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes());

        let mut system = System::new(Bios::stub());
        system.bus.ram.write32(0x0005_0000, 0xFFFF_FFFF);
        system.load_exe(&data).unwrap();
        assert_eq!(system.bus.ram.read32(0x0005_0000), 0);
    }

    #[test]
    fn diagnostics_start_clean() {
        let system = System::new(Bios::stub());
        assert_eq!(system.diagnostics(), Diagnostics::default());
    }

    #[test]
    fn reset_returns_the_cpu_to_the_bios_entry_point() {
        let mut system = system_running(&[0x1000_FFFF, 0x0000_0000]);
        system.run_frame();
        system.reset();
        assert_eq!(system.cpu.pc(), crate::cpu::RESET_VECTOR);
    }

    #[test]
    fn pal_runs_more_cycles_per_frame_than_ntsc() {
        let mut system = system_running(&[0x1000_FFFF, 0x0000_0000]);
        let ntsc = system.run_frame().cycles;

        let mut system = system_running(&[0x1000_FFFF, 0x0000_0000]);
        system.set_region(Region::Pal);
        let pal = system.run_frame().cycles;

        assert!(pal > ntsc, "PAL: {pal}, NTSC: {ntsc}");
    }
}

#[cfg(test)]
mod disc_tests {
    use super::*;
    use crate::cdrom::SECTOR_USER;

    #[test]
    fn loading_an_iso_makes_the_drive_report_a_disc() {
        let mut system = System::new(Bios::stub());
        assert!(system.disc().is_none());

        system
            .load_disc(vec![0u8; SECTOR_USER * 32])
            .expect("ISO válido");

        assert_eq!(system.disc().map(|disc| disc.total_sectors()), Some(32));
    }

    #[test]
    fn a_cue_sheet_describes_the_tracks() {
        let mut system = System::new(Bios::stub());
        let cue = "FILE \"j.bin\" BINARY\n TRACK 01 MODE1/2048\n  INDEX 01 00:00:00\n";

        system
            .load_disc_with_cue(cue, vec![0u8; SECTOR_USER * 8])
            .expect("CUE válido");

        assert_eq!(system.disc().map(|disc| disc.tracks().len()), Some(1));
    }

    #[test]
    fn reset_keeps_the_disc_in_the_tray() {
        let mut system = System::new(Bios::stub());
        system.load_disc(vec![0u8; SECTOR_USER * 8]).unwrap();

        system.reset();

        assert!(
            system.disc().is_some(),
            "apertar reset no console não abre a bandeja"
        );
    }

    #[test]
    fn ejecting_empties_the_tray() {
        let mut system = System::new(Bios::stub());
        system.load_disc(vec![0u8; SECTOR_USER * 8]).unwrap();

        system.eject_disc();

        assert!(system.disc().is_none());
    }

    #[test]
    fn a_malformed_image_is_reported_instead_of_silently_ignored() {
        let mut system = System::new(Bios::stub());
        let error = system.load_disc(vec![0u8; 1234]).unwrap_err();
        assert!(matches!(error, PsxError::Disc(_)), "erro foi {error:?}");
    }
}
