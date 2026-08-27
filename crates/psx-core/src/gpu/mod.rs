//! GPU do PlayStation.
//!
//! Referência: PSX-SPX — "GPU", "GPU I/O Ports", "GPU Status Register",
//! "GPU Render Commands", "GPU Display Control", "GPU Memory Transfer Commands".
//!
//! A GPU tem exatamente duas portas:
//!
//! | Endereço      | Escrita | Leitura   |
//! |---------------|---------|-----------|
//! | `0x1F80_1810` | `GP0`   | `GPUREAD` |
//! | `0x1F80_1814` | `GP1`   | `GPUSTAT` |

mod rasterizer;
mod vram;

pub use rasterizer::{Attributes, Color, DrawState, TextureDepth, TransparencyMode, Vertex};
pub use vram::{bgr555_to_rgba8, Vram, VRAM_HEIGHT, VRAM_WIDTH};

use crate::irq::{Interrupt, IrqController};

/// Recorte do estado de display e de desenho, para diagnóstico.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisplayState {
    pub disabled: bool,
    pub vram_x: u32,
    pub vram_y: u32,
    pub width: u32,
    pub height: u32,
    pub depth_24: bool,
    pub interlaced: bool,
    /// `(left, top, right, bottom)` da área de desenho.
    pub draw_area: (i32, i32, i32, i32),
    pub draw_offset: (i32, i32),
}

/// Direção do DMA configurada por `GP1(0x04)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DmaDirection {
    #[default]
    Off,
    Fifo,
    CpuToGp0,
    VramToCpu,
}

/// Resolução horizontal, codificada em dois campos de `GPUSTAT`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HorizontalRes(u8);

impl HorizontalRes {
    /// Constrói a partir dos campos `hr1` (2 bits) e `hr2` (1 bit) de `GP1(0x08)`.
    pub const fn from_fields(hr1: u8, hr2: u8) -> Self {
        Self((hr2 & 1) << 2 | (hr1 & 3))
    }

    /// Bits 16..18 de `GPUSTAT`.
    pub const fn status_bits(self) -> u32 {
        let hr1 = (self.0 & 3) as u32;
        let hr2 = ((self.0 >> 2) & 1) as u32;
        (hr2 << 16) | (hr1 << 17)
    }

    pub const fn pixels(self) -> u32 {
        // O bit 2 (368 px) tem prioridade sobre os outros.
        if self.0 & 4 != 0 {
            368
        } else {
            match self.0 & 3 {
                0 => 256,
                1 => 320,
                2 => 512,
                _ => 640,
            }
        }
    }
}

impl Default for HorizontalRes {
    fn default() -> Self {
        // 320×240 é o modo em que a BIOS inicializa.
        Self::from_fields(1, 0)
    }
}

/// Estado do parser de GP0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Gp0Mode {
    /// Acumulando palavras de um comando.
    Command,
    /// Recebendo dados de uma transferência CPU→VRAM.
    CpuToVram,
    /// Acumulando vértices de uma polilinha até o word de terminação.
    Polyline {
        shaded: bool,
        semi_transparent: bool,
    },
}

/// Transferência retangular em andamento entre CPU e VRAM.
#[derive(Debug, Clone, Copy, Default)]
struct Transfer {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    /// Quantos halfwords já foram transferidos.
    progress: u32,
}

impl Transfer {
    const fn total_halfwords(&self) -> u32 {
        self.width * self.height
    }

    const fn is_done(&self) -> bool {
        self.progress >= self.total_halfwords()
    }

    /// Coordenada do próximo halfword, com wrap dentro do retângulo.
    const fn next_position(&self) -> (i32, i32) {
        let x = self.x + (self.progress % self.width);
        let y = self.y + (self.progress / self.width);
        (x as i32, y as i32)
    }
}

/// Largura máxima que o display pode ter (modo 640).
pub const FRAME_WIDTH_MAX: usize = 640;
/// Altura máxima que o display pode ter (480 entrelaçado, com folga).
pub const FRAME_HEIGHT_MAX: usize = 512;

/// A GPU.
pub struct Gpu {
    vram: Vram,
    draw: DrawState,

    // ------------------------------------------------------------- display
    display_vram_x: u32,
    display_vram_y: u32,
    display_horizontal_start: u32,
    display_horizontal_end: u32,
    display_line_start: u32,
    display_line_end: u32,
    horizontal_res: HorizontalRes,
    /// `false` = 240 linhas, `true` = 480 (só com entrelaçamento).
    vertical_res_480: bool,
    pal_mode: bool,
    display_depth_24: bool,
    interlaced: bool,
    display_disabled: bool,
    /// Campo atual no modo entrelaçado.
    odd_field: bool,

    // -------------------------------------------------------------- status
    dma_direction: DmaDirection,
    irq_requested: bool,
    rectangle_texture_x_flip: bool,
    rectangle_texture_y_flip: bool,
    /// `GP1(0x09)` — permite que `GP0(0xE1).11` desabilite texturas.
    texture_disable_allowed: bool,
    /// Últimos bits de texpage vindos de `GP0(0xE1)`, refletidos em `GPUSTAT`.
    texpage_status_bits: u32,

    // ------------------------------------------------------------ parsing
    command: Vec<u32>,
    command_length: usize,
    mode: Gp0Mode,
    transfer: Transfer,
    /// `true` enquanto uma transferência VRAM→CPU alimenta `GPUREAD`.
    reading_vram: bool,
    /// Valor devolvido por `GPUREAD` fora de uma transferência VRAM→CPU.
    gpuread_latch: u32,
    /// Último vértice de uma polilinha em andamento.
    polyline_previous: Option<Vertex>,
    /// Cor já lida do stream mas ainda sem vértice (polilinha Gouraud).
    polyline_pending_color: Option<Color>,

    // ------------------------------------------------------------- saída
    framebuffer: Vec<u8>,
    frame_width: u32,
    frame_height: u32,

    /// Comandos GP0 recebidos sem tratamento, para diagnóstico.
    unhandled_commands: u64,
}

impl Gpu {
    pub fn new() -> Self {
        let mut gpu = Self {
            vram: Vram::new(),
            draw: DrawState::default(),
            display_vram_x: 0,
            display_vram_y: 0,
            display_horizontal_start: 0x200,
            display_horizontal_end: 0xC00,
            display_line_start: 0x10,
            display_line_end: 0x100,
            horizontal_res: HorizontalRes::default(),
            vertical_res_480: false,
            pal_mode: false,
            display_depth_24: false,
            interlaced: false,
            display_disabled: true,
            odd_field: false,
            dma_direction: DmaDirection::Off,
            irq_requested: false,
            rectangle_texture_x_flip: false,
            rectangle_texture_y_flip: false,
            texture_disable_allowed: false,
            texpage_status_bits: 0,
            command: Vec::with_capacity(16),
            command_length: 0,
            mode: Gp0Mode::Command,
            transfer: Transfer::default(),
            reading_vram: false,
            gpuread_latch: 0,
            polyline_previous: None,
            polyline_pending_color: None,
            framebuffer: vec![0; FRAME_WIDTH_MAX * FRAME_HEIGHT_MAX * 4],
            frame_width: 320,
            frame_height: 240,
            unhandled_commands: 0,
        };
        gpu.reset();
        gpu
    }

    /// `GP1(0x00)` — reset completo.
    pub fn reset(&mut self) {
        self.draw = DrawState::default();
        self.display_vram_x = 0;
        self.display_vram_y = 0;
        self.display_horizontal_start = 0x200;
        self.display_horizontal_end = 0xC00;
        self.display_line_start = 0x10;
        self.display_line_end = 0x100;
        self.horizontal_res = HorizontalRes::default();
        self.vertical_res_480 = false;
        self.display_depth_24 = false;
        self.interlaced = false;
        self.display_disabled = true;
        self.dma_direction = DmaDirection::Off;
        self.irq_requested = false;
        self.rectangle_texture_x_flip = false;
        self.rectangle_texture_y_flip = false;
        self.texture_disable_allowed = false;
        self.texpage_status_bits = 0;
        self.reset_command_buffer();
    }

    fn reset_command_buffer(&mut self) {
        self.command.clear();
        self.command_length = 0;
        self.mode = Gp0Mode::Command;
        self.polyline_previous = None;
        self.polyline_pending_color = None;
        self.reading_vram = false;
        self.transfer = Transfer::default();
    }

    pub fn vram(&self) -> &Vram {
        &self.vram
    }

    pub fn vram_mut(&mut self) -> &mut Vram {
        &mut self.vram
    }

    pub const fn unhandled_commands(&self) -> u64 {
        self.unhandled_commands
    }

    // ------------------------------------------------------------- GPUSTAT

    /// Lê `GPUSTAT` (`0x1F80_1814`).
    pub fn status(&self) -> u32 {
        let mut status = self.texpage_status_bits;

        status |= (self.draw.force_mask_bit as u32) << 11;
        status |= (self.draw.check_mask_bit as u32) << 12;
        status |= (self.odd_field as u32) << 13;
        // Bit 14 ("reverse flag") não é usado por software comercial.
        status |= self.horizontal_res.status_bits();
        status |= (self.vertical_res_480 as u32) << 19;
        status |= (self.pal_mode as u32) << 20;
        status |= (self.display_depth_24 as u32) << 21;
        status |= (self.interlaced as u32) << 22;
        status |= (self.display_disabled as u32) << 23;
        status |= (self.irq_requested as u32) << 24;

        // Bits 26..28 anunciam prontidão. O interpretador termina cada comando
        // instantaneamente, então estamos sempre prontos — exceto para enviar
        // VRAM quando não há transferência de leitura ativa.
        status |= 1 << 26; // pronto para receber comando
        status |= (matches!(self.mode, Gp0Mode::Command) as u32) << 27;
        status |= 1 << 28; // pronto para receber bloco de DMA

        status |= (self.dma_direction as u32) << 29;

        // Bit 25 espelha a linha de "data request" conforme a direção do DMA.
        let data_request = match self.dma_direction {
            DmaDirection::Off => 0,
            DmaDirection::Fifo => 1,
            DmaDirection::CpuToGp0 => (status >> 28) & 1,
            DmaDirection::VramToCpu => (status >> 27) & 1,
        };
        status |= data_request << 25;

        // Bit 31: linha ímpar sendo desenhada. Em 480i acompanha o campo.
        status |= (self.odd_field as u32) << 31;

        status
    }

    /// Lê `GPUREAD` (`0x1F80_1810`).
    ///
    /// Durante uma transferência VRAM→CPU cada leitura entrega dois pixels de
    /// 16 bits. Fora dela, devolve o latch deixado por `GP1(0x10)`.
    pub fn read(&mut self) -> u32 {
        if self.reading_vram {
            let low = self.next_vram_word() as u32;
            let high = self.next_vram_word() as u32;
            self.gpuread_latch = low | (high << 16);
        }
        self.gpuread_latch
    }

    fn next_vram_word(&mut self) -> u16 {
        if self.transfer.is_done() {
            return 0;
        }
        let (x, y) = self.transfer.next_position();
        self.transfer.progress += 1;
        let value = self.vram.get(x, y);
        if self.transfer.is_done() {
            self.reading_vram = false;
        }
        value
    }

    // ----------------------------------------------------------------- GP1

    /// Escrita em `GP1` (`0x1F80_1814`) — comandos de display e controle.
    ///
    /// `GP1(0x02)` só limpa o flag interno de IRQ da GPU; o bit correspondente
    /// em `I_STAT` é reconhecido separadamente pela CPU.
    pub fn write_gp1(&mut self, word: u32) {
        let command = word >> 24;
        let payload = word & 0x00FF_FFFF;

        match command {
            0x00 => self.reset(),
            0x01 => self.reset_command_buffer(),
            0x02 => self.irq_requested = false,
            0x03 => self.display_disabled = payload & 1 != 0,
            0x04 => {
                self.dma_direction = match payload & 3 {
                    0 => DmaDirection::Off,
                    1 => DmaDirection::Fifo,
                    2 => DmaDirection::CpuToGp0,
                    _ => DmaDirection::VramToCpu,
                }
            }
            0x05 => {
                self.display_vram_x = payload & 0x3FE;
                self.display_vram_y = (payload >> 10) & 0x1FF;
            }
            0x06 => {
                self.display_horizontal_start = payload & 0xFFF;
                self.display_horizontal_end = (payload >> 12) & 0xFFF;
            }
            0x07 => {
                self.display_line_start = payload & 0x3FF;
                self.display_line_end = (payload >> 10) & 0x3FF;
            }
            0x08 => {
                self.horizontal_res =
                    HorizontalRes::from_fields((payload & 3) as u8, ((payload >> 6) & 1) as u8);
                self.vertical_res_480 = payload & 0x04 != 0;
                self.pal_mode = payload & 0x08 != 0;
                self.display_depth_24 = payload & 0x10 != 0;
                self.interlaced = payload & 0x20 != 0;
            }
            0x09 => self.texture_disable_allowed = payload & 1 != 0,
            0x10..=0x1F => self.gpuread_latch = self.info(payload),
            _ => self.unhandled_commands += 1,
        }
    }

    /// `GP1(0x10)` — devolve estado interno pela porta `GPUREAD`.
    fn info(&self, payload: u32) -> u32 {
        match payload & 0x0F {
            2 => {
                (self.draw.texture_window_mask_x as u32)
                    | ((self.draw.texture_window_mask_y as u32) << 5)
                    | ((self.draw.texture_window_offset_x as u32) << 10)
                    | ((self.draw.texture_window_offset_y as u32) << 15)
            }
            3 => (self.draw.area_left as u32) | ((self.draw.area_top as u32) << 10),
            4 => (self.draw.area_right as u32) | ((self.draw.area_bottom as u32) << 10),
            5 => {
                ((self.draw.offset_x as u32) & 0x7FF)
                    | (((self.draw.offset_y as u32) & 0x7FF) << 11)
            }
            7 => 2, // versão da GPU
            _ => self.gpuread_latch,
        }
    }

    // ----------------------------------------------------------------- GP0

    /// Escrita em `GP0` (`0x1F80_1810`) — comandos de desenho e transferência.
    pub fn write_gp0(&mut self, word: u32, irq: &mut IrqController) {
        match self.mode {
            Gp0Mode::CpuToVram => {
                self.push_vram_word(word as u16);
                self.push_vram_word((word >> 16) as u16);
                if self.transfer.is_done() {
                    self.mode = Gp0Mode::Command;
                }
            }
            Gp0Mode::Polyline {
                shaded,
                semi_transparent,
            } => self.push_polyline_word(word, shaded, semi_transparent),
            Gp0Mode::Command => {
                if self.command.is_empty() {
                    self.command_length = gp0_command_length(word);
                }
                self.command.push(word);
                if self.command.len() >= self.command_length {
                    self.execute_gp0(irq);
                }
            }
        }
    }

    fn push_vram_word(&mut self, value: u16) {
        if self.transfer.is_done() {
            return;
        }
        let (x, y) = self.transfer.next_position();
        self.transfer.progress += 1;
        self.vram.set(x, y, value);
    }

    fn execute_gp0(&mut self, irq: &mut IrqController) {
        let word = self.command[0];
        let opcode = (word >> 24) as u8;

        match opcode {
            0x00 => {}
            0x01 => { /* limpar cache de textura: não modelamos a cache */ }
            0x02 => self.fill_rectangle(),
            0x1F => {
                self.irq_requested = true;
                irq.raise(Interrupt::Gpu);
            }
            0x20..=0x3F => self.draw_polygon(),
            0x40..=0x5F => self.draw_line_command(),
            0x60..=0x7F => self.draw_rect_command(),
            0x80..=0x9F => self.vram_to_vram(),
            0xA0..=0xBF => self.begin_cpu_to_vram(),
            0xC0..=0xDF => self.begin_vram_to_cpu(),
            0xE1 => self.set_draw_mode(word),
            0xE2 => self.set_texture_window(word),
            0xE3 => {
                self.draw.area_left = (word & 0x3FF) as i32;
                self.draw.area_top = ((word >> 10) & 0x1FF) as i32;
            }
            0xE4 => {
                self.draw.area_right = (word & 0x3FF) as i32;
                self.draw.area_bottom = ((word >> 10) & 0x1FF) as i32;
            }
            0xE5 => {
                // Offsets são de 11 bits com sinal.
                self.draw.offset_x = sign_extend_11(word & 0x7FF);
                self.draw.offset_y = sign_extend_11((word >> 11) & 0x7FF);
            }
            0xE6 => {
                self.draw.force_mask_bit = word & 1 != 0;
                self.draw.check_mask_bit = word & 2 != 0;
            }
            _ => self.unhandled_commands += 1,
        }

        // Comandos de transferência e polilinha trocam `self.mode` e passam a
        // consumir o stream diretamente; em todos os casos o buffer do comando
        // atual já foi consumido.
        self.command.clear();
        self.command_length = 0;
    }

    fn set_draw_mode(&mut self, word: u32) {
        self.draw.texpage_x = ((word & 0x0F) * 64) as i32;
        self.draw.texpage_y = (((word >> 4) & 1) * 256) as i32;
        self.draw.transparency = TransparencyMode::from_bits((word >> 5) & 3);
        self.draw.texture_depth = TextureDepth::from_bits((word >> 7) & 3);
        self.draw.dither = word & (1 << 9) != 0;
        self.rectangle_texture_x_flip = word & (1 << 12) != 0;
        self.rectangle_texture_y_flip = word & (1 << 13) != 0;
        self.draw.texture_disable = self.texture_disable_allowed && word & (1 << 11) != 0;

        // Bits 0..10 e 15 de GPUSTAT são um espelho direto deste comando.
        self.texpage_status_bits = (word & 0x07FF) | ((word & (1 << 11)) << 4);
    }

    fn set_texture_window(&mut self, word: u32) {
        self.draw.texture_window_mask_x = (word & 0x1F) as u8;
        self.draw.texture_window_mask_y = ((word >> 5) & 0x1F) as u8;
        self.draw.texture_window_offset_x = ((word >> 10) & 0x1F) as u8;
        self.draw.texture_window_offset_y = ((word >> 15) & 0x1F) as u8;
    }

    /// `GP0(0x02)` — preenche um retângulo ignorando área de desenho e máscara.
    fn fill_rectangle(&mut self) {
        let color = Color::from_command(self.command[0]);
        // O hardware alinha X em múltiplos de 16 e a largura para cima.
        let x = (self.command[1] & 0x3F0) as i32;
        let y = ((self.command[1] >> 16) & 0x1FF) as i32;
        let width = (((self.command[2] & 0x3FF) + 0x0F) & !0x0F) as i32;
        let height = ((self.command[2] >> 16) & 0x1FF) as i32;

        let packed =
            (color.r as u16 >> 3) | ((color.g as u16 >> 3) << 5) | ((color.b as u16 >> 3) << 10);

        for row in 0..height {
            for column in 0..width {
                self.vram.set(x + column, y + row, packed);
            }
        }
    }

    fn draw_polygon(&mut self) {
        let word = self.command[0];
        let opcode = word >> 24;
        let gouraud = opcode & 0x10 != 0;
        let quad = opcode & 0x08 != 0;
        let textured = opcode & 0x04 != 0;
        let semi_transparent = opcode & 0x02 != 0;
        let raw_texture = opcode & 0x01 != 0;

        let count = if quad { 4 } else { 3 };
        let base_color = Color::from_command(word);

        let mut vertices = [Vertex::default(); 4];
        let mut cursor = 1usize;
        let mut clut = 0u32;
        let mut texpage = 0u32;

        for (index, vertex) in vertices.iter_mut().enumerate().take(count) {
            // Em Gouraud, cada vértice a partir do segundo traz sua própria
            // cor antes das coordenadas. Em modo `raw texture` a cor é
            // ignorada pelo rasterizador.
            let color = if gouraud && index > 0 {
                let c = Color::from_command(self.command[cursor]);
                cursor += 1;
                c
            } else {
                base_color
            };

            *vertex = Vertex::from_command(self.command[cursor]);
            cursor += 1;
            vertex.color = color;

            if textured {
                let attribute = self.command[cursor];
                cursor += 1;
                vertex.u = attribute as u8;
                vertex.v = (attribute >> 8) as u8;
                match index {
                    0 => clut = attribute >> 16,
                    1 => texpage = attribute >> 16,
                    _ => {}
                }
            }
        }

        if textured {
            self.draw.clut_x = ((clut & 0x3F) * 16) as i32;
            self.draw.clut_y = ((clut >> 6) & 0x1FF) as i32;
            self.apply_texpage(texpage);
        }

        let attributes = Attributes {
            shaded: gouraud,
            textured,
            raw_texture,
            semi_transparent,
        };

        rasterizer::draw_triangle(
            &mut self.vram,
            &self.draw,
            &[vertices[0], vertices[1], vertices[2]],
            &attributes,
        );
        if quad {
            rasterizer::draw_triangle(
                &mut self.vram,
                &self.draw,
                &[vertices[1], vertices[2], vertices[3]],
                &attributes,
            );
        }
    }

    /// Aplica os bits de texpage embutidos no atributo do segundo vértice.
    fn apply_texpage(&mut self, texpage: u32) {
        self.draw.texpage_x = ((texpage & 0x0F) * 64) as i32;
        self.draw.texpage_y = (((texpage >> 4) & 1) * 256) as i32;
        self.draw.transparency = TransparencyMode::from_bits((texpage >> 5) & 3);
        self.draw.texture_depth = TextureDepth::from_bits((texpage >> 7) & 3);
        self.texpage_status_bits = (self.texpage_status_bits & !0x01FF) | (texpage & 0x01FF);
    }

    fn draw_rect_command(&mut self) {
        let word = self.command[0];
        let opcode = word >> 24;
        let size_field = (opcode >> 3) & 3;
        let textured = opcode & 0x04 != 0;
        let semi_transparent = opcode & 0x02 != 0;
        let raw_texture = opcode & 0x01 != 0;

        let color = if textured && raw_texture {
            Color::NEUTRAL
        } else {
            Color::from_command(word)
        };

        let mut cursor = 1usize;
        let mut origin = Vertex::from_command(self.command[cursor]);
        cursor += 1;
        origin.color = color;

        if textured {
            let attribute = self.command[cursor];
            cursor += 1;
            origin.u = attribute as u8;
            origin.v = (attribute >> 8) as u8;
            let clut = attribute >> 16;
            self.draw.clut_x = ((clut & 0x3F) * 16) as i32;
            self.draw.clut_y = ((clut >> 6) & 0x1FF) as i32;
        }

        let size = match size_field {
            1 => (1, 1),
            2 => (8, 8),
            3 => (16, 16),
            _ => {
                let word = self.command[cursor];
                (((word & 0x3FF) as i32), (((word >> 16) & 0x1FF) as i32))
            }
        };

        rasterizer::draw_rectangle(
            &mut self.vram,
            &self.draw,
            &origin,
            size,
            &Attributes {
                shaded: false,
                textured,
                raw_texture,
                semi_transparent,
            },
        );
    }

    fn draw_line_command(&mut self) {
        let word = self.command[0];
        let opcode = word >> 24;
        let gouraud = opcode & 0x10 != 0;
        let polyline = opcode & 0x08 != 0;
        let semi_transparent = opcode & 0x02 != 0;

        if polyline {
            // Nos dois modos o cabeçalho é `comando+cor1` seguido de `vértice1`.
            // O restante (`cor_n`, `vértice_n`, ...) vem pelo stream até o
            // terminador `0x5___5___`.
            let mut first = Vertex::from_command(self.command[1]);
            first.color = Color::from_command(word);
            self.polyline_previous = Some(first);
            self.polyline_pending_color = None;
            self.mode = Gp0Mode::Polyline {
                shaded: gouraud,
                semi_transparent,
            };
            return;
        }

        let (from, to) = if gouraud {
            let mut a = Vertex::from_command(self.command[1]);
            a.color = Color::from_command(word);
            let mut b = Vertex::from_command(self.command[3]);
            b.color = Color::from_command(self.command[2]);
            (a, b)
        } else {
            let color = Color::from_command(word);
            let mut a = Vertex::from_command(self.command[1]);
            a.color = color;
            let mut b = Vertex::from_command(self.command[2]);
            b.color = color;
            (a, b)
        };

        rasterizer::draw_line(
            &mut self.vram,
            &self.draw,
            &from,
            &to,
            gouraud,
            semi_transparent,
        );
    }

    fn push_polyline_word(&mut self, word: u32, shaded: bool, semi_transparent: bool) {
        // Terminador: qualquer word cujos nibbles altos formem 0x5___5___.
        if word & 0xF000_F000 == 0x5000_5000 {
            self.polyline_previous = None;
            self.polyline_pending_color = None;
            self.mode = Gp0Mode::Command;
            return;
        }

        if shaded && self.polyline_pending_color.is_none() {
            self.polyline_pending_color = Some(Color::from_command(word));
            return;
        }

        let color = self
            .polyline_pending_color
            .take()
            .or_else(|| self.polyline_previous.map(|v| v.color))
            .unwrap_or_default();

        let mut next = Vertex::from_command(word);
        next.color = color;

        if let Some(previous) = self.polyline_previous {
            rasterizer::draw_line(
                &mut self.vram,
                &self.draw,
                &previous,
                &next,
                shaded,
                semi_transparent,
            );
        }
        self.polyline_previous = Some(next);
    }

    fn vram_to_vram(&mut self) {
        let source_x = (self.command[1] & 0x3FF) as i32;
        let source_y = ((self.command[1] >> 16) & 0x1FF) as i32;
        let destination_x = (self.command[2] & 0x3FF) as i32;
        let destination_y = ((self.command[2] >> 16) & 0x1FF) as i32;
        let width = wrap_size(self.command[3] & 0xFFFF, 0x400) as i32;
        let height = wrap_size((self.command[3] >> 16) & 0xFFFF, 0x200) as i32;

        for row in 0..height {
            for column in 0..width {
                let pixel = self.vram.get(source_x + column, source_y + row);
                let destination = self.vram.get(destination_x + column, destination_y + row);
                if self.draw.check_mask_bit && destination & 0x8000 != 0 {
                    continue;
                }
                let pixel = if self.draw.force_mask_bit {
                    pixel | 0x8000
                } else {
                    pixel
                };
                self.vram
                    .set(destination_x + column, destination_y + row, pixel);
            }
        }
    }

    fn begin_cpu_to_vram(&mut self) {
        self.transfer = Transfer {
            x: self.command[1] & 0x3FF,
            y: (self.command[1] >> 16) & 0x1FF,
            width: wrap_size(self.command[2] & 0xFFFF, 0x400),
            height: wrap_size((self.command[2] >> 16) & 0xFFFF, 0x200),
            progress: 0,
        };
        self.reading_vram = false;
        if self.transfer.total_halfwords() == 0 {
            self.mode = Gp0Mode::Command;
        } else {
            self.mode = Gp0Mode::CpuToVram;
        }
    }

    fn begin_vram_to_cpu(&mut self) {
        self.transfer = Transfer {
            x: self.command[1] & 0x3FF,
            y: (self.command[1] >> 16) & 0x1FF,
            width: wrap_size(self.command[2] & 0xFFFF, 0x400),
            height: wrap_size((self.command[2] >> 16) & 0xFFFF, 0x200),
            progress: 0,
        };
        self.reading_vram = self.transfer.total_halfwords() > 0;
        self.mode = Gp0Mode::Command;
    }

    // ------------------------------------------------------------- vídeo

    /// Marca o fim de um frame: alterna o campo do entrelaçamento e produz o
    /// framebuffer RGBA8 a partir da área de display atual.
    pub fn end_of_frame(&mut self) {
        if self.interlaced {
            self.odd_field = !self.odd_field;
        } else {
            self.odd_field = false;
        }
        self.render_display();
    }

    fn render_display(&mut self) {
        let width = self.horizontal_res.pixels();
        let height = if self.vertical_res_480 && self.interlaced {
            480
        } else {
            240
        };
        self.frame_width = width;
        self.frame_height = height;

        if self.display_disabled {
            self.framebuffer[..(width * height * 4) as usize].fill(0);
            return;
        }

        for row in 0..height {
            let vram_y = (self.display_vram_y + row) as i32;
            for column in 0..width {
                let index = ((row * width + column) * 4) as usize;
                let rgba = if self.display_depth_24 {
                    self.sample_24bit(column, vram_y)
                } else {
                    let x = (self.display_vram_x + column) as i32;
                    bgr555_to_rgba8(self.vram.get(x, vram_y))
                };
                self.framebuffer[index..index + 4].copy_from_slice(&rgba);
            }
        }
    }

    /// No modo 24 bpp cada pixel ocupa 3 bytes, então 2 pixels de VRAM cobrem
    /// 1⅓ pixel de tela. PSX-SPX — "GPU Display Control", `Display Area Color Depth`.
    fn sample_24bit(&self, column: u32, vram_y: i32) -> [u8; 4] {
        let byte_offset = column * 3;
        let x = self.display_vram_x + byte_offset / 2;
        let first = self.vram.get(x as i32, vram_y);
        let second = self.vram.get((x + 1) as i32, vram_y);
        let bytes = [
            first as u8,
            (first >> 8) as u8,
            second as u8,
            (second >> 8) as u8,
        ];
        let start = (byte_offset % 2) as usize;
        [bytes[start], bytes[start + 1], bytes[start + 2], 0xFF]
    }

    /// Framebuffer RGBA8 do último frame. O comprimento útil é
    /// `frame_width * frame_height * 4`.
    /// Estado da janela de display, para diagnóstico.
    ///
    /// Quando a VRAM tem conteúdo mas a tela está preta, a resposta está aqui:
    /// ou o display está desligado, ou a janela aponta para outra região.
    pub fn display_state(&self) -> DisplayState {
        DisplayState {
            disabled: self.display_disabled,
            vram_x: self.display_vram_x,
            vram_y: self.display_vram_y,
            width: self.frame_width,
            height: self.frame_height,
            depth_24: self.display_depth_24,
            interlaced: self.interlaced,
            draw_area: (
                self.draw.area_left,
                self.draw.area_top,
                self.draw.area_right,
                self.draw.area_bottom,
            ),
            draw_offset: (self.draw.offset_x, self.draw.offset_y),
        }
    }

    pub fn framebuffer(&self) -> &[u8] {
        &self.framebuffer
    }

    pub const fn frame_width(&self) -> u32 {
        self.frame_width
    }

    pub const fn frame_height(&self) -> u32 {
        self.frame_height
    }

    pub const fn dma_direction(&self) -> DmaDirection {
        self.dma_direction
    }
}

impl Default for Gpu {
    fn default() -> Self {
        Self::new()
    }
}

/// Tamanhos de transferência: 0 significa o máximo da dimensão.
const fn wrap_size(value: u32, max: u32) -> u32 {
    let value = value & (max - 1);
    if value == 0 {
        max
    } else {
        value
    }
}

const fn sign_extend_11(value: u32) -> i32 {
    ((value as i32) << 21) >> 21
}

/// Quantas palavras um comando GP0 consome, incluindo a própria palavra de
/// comando. PSX-SPX — "GPU Render Commands".
fn gp0_command_length(word: u32) -> usize {
    let opcode = word >> 24;
    match opcode {
        0x02 => 3,
        0x20..=0x3F => {
            let vertices = if opcode & 0x08 != 0 { 4 } else { 3 };
            let textured = (opcode & 0x04 != 0) as usize;
            let gouraud = opcode & 0x10 != 0;
            1 + vertices * (1 + textured) + if gouraud { vertices - 1 } else { 0 }
        }
        0x40..=0x5F => {
            if opcode & 0x08 != 0 {
                // Polilinha: o cabeçalho é `comando+cor` e o primeiro vértice;
                // o resto vem pelo stream até o terminador.
                2
            } else if opcode & 0x10 != 0 {
                4
            } else {
                3
            }
        }
        0x60..=0x7F => {
            let textured = (opcode & 0x04 != 0) as usize;
            let variable_size = ((opcode >> 3) & 3) == 0;
            2 + textured + variable_size as usize
        }
        0x80..=0x9F => 4,
        0xA0..=0xDF => 3,
        _ => 1,
    }
}
