//! Software rasterizer da GPU.
//!
//! Referência: PSX-SPX — "GPU Render Polygon Commands",
//! "GPU Render Line Commands", "GPU Render Rectangle Commands",
//! "GPU Texture Caching", "GPU Semi Transparency".
//!
//! O rasterizador é a implementação de referência: é ele que define o que é
//! "correto" no projeto. Qualquer backend acelerado adicionado depois precisa
//! bater com a saída daqui.
//!
//! Nota sobre precisão: a interpolação de cor e de UV usa coordenadas
//! baricêntricas inteiras. O hardware usa um interpolador ligeiramente
//! diferente, o que produz divergências de ±1 em gradientes longos. Está
//! documentado como divergência conhecida em `docs/architecture.md`.

use super::vram::Vram;

/// Cor RGB de 8 bits por canal (como vem nos comandos GP0).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Color {
    /// Extrai a cor dos 24 bits baixos de uma palavra de comando.
    pub const fn from_command(word: u32) -> Self {
        Self {
            r: word as u8,
            g: (word >> 8) as u8,
            b: (word >> 16) as u8,
        }
    }

    /// Cor neutra para blending de textura (`0x80` = fator 1.0).
    pub const NEUTRAL: Color = Color {
        r: 0x80,
        g: 0x80,
        b: 0x80,
    };
}

/// Vértice já em coordenadas de VRAM, com cor e coordenada de textura.
#[derive(Debug, Clone, Copy, Default)]
pub struct Vertex {
    pub x: i32,
    pub y: i32,
    pub color: Color,
    pub u: u8,
    pub v: u8,
}

impl Vertex {
    /// Decodifica o par `YYYYXXXX` de uma palavra de comando.
    ///
    /// As coordenadas são de 11 bits com sinal.
    pub const fn from_command(word: u32) -> Self {
        let x = ((word & 0xFFFF) as u16 as i16) << 5 >> 5;
        let y = ((word >> 16) as u16 as i16) << 5 >> 5;
        Self {
            x: x as i32,
            y: y as i32,
            color: Color { r: 0, g: 0, b: 0 },
            u: 0,
            v: 0,
        }
    }
}

/// Profundidade de cor da texture page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TextureDepth {
    /// 4 bits por pixel, indexado via CLUT.
    #[default]
    Bpp4,
    /// 8 bits por pixel, indexado via CLUT.
    Bpp8,
    /// 15 bits por pixel, cor direta.
    Bpp15,
}

impl TextureDepth {
    pub const fn from_bits(bits: u32) -> Self {
        match bits & 3 {
            0 => TextureDepth::Bpp4,
            1 => TextureDepth::Bpp8,
            // O modo 3 é "reservado" e se comporta como 15bpp.
            _ => TextureDepth::Bpp15,
        }
    }
}

/// Modo de mistura para pixels semi-transparentes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TransparencyMode {
    /// `B/2 + F/2`
    #[default]
    Half,
    /// `B + F`
    Add,
    /// `B - F`
    Subtract,
    /// `B + F/4`
    AddQuarter,
}

impl TransparencyMode {
    pub const fn from_bits(bits: u32) -> Self {
        match bits & 3 {
            0 => TransparencyMode::Half,
            1 => TransparencyMode::Add,
            2 => TransparencyMode::Subtract,
            _ => TransparencyMode::AddQuarter,
        }
    }
}

/// Estado de desenho compartilhado por todas as primitivas.
#[derive(Debug, Clone, Copy)]
pub struct DrawState {
    pub area_left: i32,
    pub area_top: i32,
    pub area_right: i32,
    pub area_bottom: i32,
    pub offset_x: i32,
    pub offset_y: i32,

    /// Base X da texture page, em pixels de 16 bits.
    pub texpage_x: i32,
    /// Base Y da texture page.
    pub texpage_y: i32,
    pub texture_depth: TextureDepth,
    pub transparency: TransparencyMode,

    /// Coordenadas da CLUT em VRAM.
    pub clut_x: i32,
    pub clut_y: i32,

    pub texture_window_mask_x: u8,
    pub texture_window_mask_y: u8,
    pub texture_window_offset_x: u8,
    pub texture_window_offset_y: u8,

    pub dither: bool,
    /// `GP0(0xE1).11` — desabilita texturas globalmente.
    pub texture_disable: bool,
    /// `GP0(0xE6).0` — força o mask bit em todo pixel escrito.
    pub force_mask_bit: bool,
    /// `GP0(0xE6).1` — não escreve sobre pixels que já têm o mask bit.
    pub check_mask_bit: bool,
}

impl Default for DrawState {
    fn default() -> Self {
        Self {
            area_left: 0,
            area_top: 0,
            area_right: 0,
            area_bottom: 0,
            offset_x: 0,
            offset_y: 0,
            texpage_x: 0,
            texpage_y: 0,
            texture_depth: TextureDepth::Bpp4,
            transparency: TransparencyMode::Half,
            clut_x: 0,
            clut_y: 0,
            texture_window_mask_x: 0,
            texture_window_mask_y: 0,
            texture_window_offset_x: 0,
            texture_window_offset_y: 0,
            dither: false,
            texture_disable: false,
            force_mask_bit: false,
            check_mask_bit: false,
        }
    }
}

/// Como a primitiva obtém a cor de cada pixel.
#[derive(Debug, Clone, Copy, Default)]
pub struct Attributes {
    /// Interpola a cor entre os vértices (Gouraud).
    pub shaded: bool,
    /// Amostra a texture page.
    pub textured: bool,
    /// Usa o texel cru, sem multiplicar pela cor do vértice.
    pub raw_texture: bool,
    /// Aplica o modo de semi-transparência.
    pub semi_transparent: bool,
}

/// Matriz de dithering 4×4 do hardware (PSX-SPX — "GPU Dithering").
const DITHER: [[i16; 4]; 4] = [
    [-4, 0, -3, 1],
    [2, -2, 3, -1],
    [-3, 1, -4, 0],
    [3, -1, 2, -2],
];

/// Desenha um triângulo preenchido.
pub fn draw_triangle(
    vram: &mut Vram,
    state: &DrawState,
    vertices: &[Vertex; 3],
    attributes: &Attributes,
) {
    let mut v = [
        offset(vertices[0], state),
        offset(vertices[1], state),
        offset(vertices[2], state),
    ];

    // O hardware descarta polígonos maiores que 1023×511.
    let min_x = v.iter().map(|p| p.x).min().unwrap();
    let max_x = v.iter().map(|p| p.x).max().unwrap();
    let min_y = v.iter().map(|p| p.y).min().unwrap();
    let max_y = v.iter().map(|p| p.y).max().unwrap();
    if max_x - min_x >= 1024 || max_y - min_y >= 512 {
        return;
    }

    let mut area = orient(&v[0], &v[1], v[2].x, v[2].y);
    if area == 0 {
        return;
    }
    if area < 0 {
        v.swap(1, 2);
        area = -area;
    }

    let left = min_x.max(state.area_left);
    let right = max_x.min(state.area_right);
    let top = min_y.max(state.area_top);
    let bottom = max_y.min(state.area_bottom);
    if left > right || top > bottom {
        return;
    }

    // Regra top-left: arestas que não são "top" nem "left" perdem o empate.
    let bias = [
        if is_top_left(&v[1], &v[2]) { 0 } else { -1 },
        if is_top_left(&v[2], &v[0]) { 0 } else { -1 },
        if is_top_left(&v[0], &v[1]) { 0 } else { -1 },
    ];

    for y in top..=bottom {
        for x in left..=right {
            let w0 = orient(&v[1], &v[2], x, y) + bias[0];
            let w1 = orient(&v[2], &v[0], x, y) + bias[1];
            let w2 = orient(&v[0], &v[1], x, y) + bias[2];

            if w0 < 0 || w1 < 0 || w2 < 0 {
                continue;
            }

            let weights = [w0 as i64, w1 as i64, w2 as i64];
            let color = if attributes.shaded {
                interpolate_color(&v, &weights, area as i64)
            } else {
                v[0].color
            };
            let (u, t) = if attributes.textured {
                interpolate_uv(&v, &weights, area as i64)
            } else {
                (0, 0)
            };

            plot(vram, state, x, y, color, (u, t), attributes);
        }
    }
}

/// Desenha um retângulo/sprite alinhado aos eixos.
pub fn draw_rectangle(
    vram: &mut Vram,
    state: &DrawState,
    origin: &Vertex,
    size: (i32, i32),
    attributes: &Attributes,
) {
    let origin = offset(*origin, state);
    let (width, height) = size;
    if width <= 0 || height <= 0 {
        return;
    }

    let left = origin.x.max(state.area_left);
    let right = (origin.x + width - 1).min(state.area_right);
    let top = origin.y.max(state.area_top);
    let bottom = (origin.y + height - 1).min(state.area_bottom);
    if left > right || top > bottom {
        return;
    }

    for y in top..=bottom {
        for x in left..=right {
            // Sprites não interpolam UV: o texel anda 1:1 com o pixel.
            let u = origin.u.wrapping_add((x - origin.x) as u8);
            let v = origin.v.wrapping_add((y - origin.y) as u8);
            plot(vram, state, x, y, origin.color, (u, v), attributes);
        }
    }
}

/// Desenha uma linha entre dois vértices (Bresenham com Gouraud).
pub fn draw_line(
    vram: &mut Vram,
    state: &DrawState,
    from: &Vertex,
    to: &Vertex,
    shaded: bool,
    semi_transparent: bool,
) {
    let a = offset(*from, state);
    let b = offset(*to, state);

    let dx = (b.x - a.x).abs();
    let dy = -(b.y - a.y).abs();
    if dx >= 1024 || -dy >= 512 {
        return;
    }

    let attributes = Attributes {
        shaded,
        textured: false,
        raw_texture: false,
        semi_transparent,
    };

    let step_x = if a.x < b.x { 1 } else { -1 };
    let step_y = if a.y < b.y { 1 } else { -1 };
    let steps = dx.max(-dy).max(1);

    let mut error = dx + dy;
    let (mut x, mut y) = (a.x, a.y);

    for i in 0..=steps {
        let color = if shaded {
            lerp_color(a.color, b.color, i, steps)
        } else {
            a.color
        };

        if x >= state.area_left
            && x <= state.area_right
            && y >= state.area_top
            && y <= state.area_bottom
        {
            plot(vram, state, x, y, color, (0, 0), &attributes);
        }

        if x == b.x && y == b.y {
            break;
        }
        let doubled = error * 2;
        if doubled >= dy {
            error += dy;
            x += step_x;
        }
        if doubled <= dx {
            error += dx;
            y += step_y;
        }
    }
}

// ----------------------------------------------------------------- internals

#[inline]
fn offset(mut vertex: Vertex, state: &DrawState) -> Vertex {
    vertex.x += state.offset_x;
    vertex.y += state.offset_y;
    vertex
}

/// Produto vetorial 2D entre `b - a` e `p - a`.
///
/// Positivo quando `p` está à esquerda da aresta `a -> b`.
#[inline]
fn orient(a: &Vertex, b: &Vertex, px: i32, py: i32) -> i32 {
    (b.x - a.x) * (py - a.y) - (b.y - a.y) * (px - a.x)
}

/// Uma aresta é "top" se for horizontal indo para a esquerda, e "left" se
/// descer. Só essas ganham o empate na regra de preenchimento.
#[inline]
fn is_top_left(a: &Vertex, b: &Vertex) -> bool {
    let edge_y = b.y - a.y;
    let edge_x = b.x - a.x;
    edge_y < 0 || (edge_y == 0 && edge_x < 0)
}

#[inline]
fn interpolate_color(v: &[Vertex; 3], w: &[i64; 3], area: i64) -> Color {
    let channel = |get: fn(&Color) -> u8| {
        let sum = w[0] * get(&v[0].color) as i64
            + w[1] * get(&v[1].color) as i64
            + w[2] * get(&v[2].color) as i64;
        (sum / area).clamp(0, 255) as u8
    };
    Color {
        r: channel(|c| c.r),
        g: channel(|c| c.g),
        b: channel(|c| c.b),
    }
}

#[inline]
fn interpolate_uv(v: &[Vertex; 3], w: &[i64; 3], area: i64) -> (u8, u8) {
    let u = (w[0] * v[0].u as i64 + w[1] * v[1].u as i64 + w[2] * v[2].u as i64) / area;
    let t = (w[0] * v[0].v as i64 + w[1] * v[1].v as i64 + w[2] * v[2].v as i64) / area;
    (u.clamp(0, 255) as u8, t.clamp(0, 255) as u8)
}

#[inline]
fn lerp_color(a: Color, b: Color, step: i32, steps: i32) -> Color {
    let mix = |x: u8, y: u8| (x as i32 + (y as i32 - x as i32) * step / steps).clamp(0, 255) as u8;
    Color {
        r: mix(a.r, b.r),
        g: mix(a.g, b.g),
        b: mix(a.b, b.b),
    }
}

/// Escreve um pixel aplicando textura, dithering, semi-transparência e mask bit.
fn plot(
    vram: &mut Vram,
    state: &DrawState,
    x: i32,
    y: i32,
    color: Color,
    uv: (u8, u8),
    attributes: &Attributes,
) {
    // Mask bit: pixels marcados não podem ser sobrescritos.
    let destination = vram.get(x, y);
    if state.check_mask_bit && destination & 0x8000 != 0 {
        return;
    }

    let mut semi_transparent = attributes.semi_transparent;
    let mut texel_mask_bit = false;

    let final_color = if attributes.textured && !state.texture_disable {
        let texel = fetch_texel(vram, state, uv.0, uv.1);
        // Texel totalmente zero é transparente e não é desenhado.
        if texel == 0 {
            return;
        }
        texel_mask_bit = texel & 0x8000 != 0;
        // Num pixel texturizado a semi-transparência só vale se o bit STP do
        // próprio texel estiver ligado.
        semi_transparent &= texel_mask_bit;

        let tex = expand_bgr555(texel);
        if attributes.raw_texture {
            tex
        } else {
            modulate(tex, color)
        }
    } else {
        color
    };

    // Dithering só se aplica a cores interpoladas ou moduladas.
    let dithered = if state.dither && !(attributes.textured && attributes.raw_texture) {
        let bias = DITHER[(y & 3) as usize][(x & 3) as usize];
        Color {
            r: (final_color.r as i16 + bias).clamp(0, 255) as u8,
            g: (final_color.g as i16 + bias).clamp(0, 255) as u8,
            b: (final_color.b as i16 + bias).clamp(0, 255) as u8,
        }
    } else {
        final_color
    };

    let mut pixel = pack_bgr555(dithered);

    if semi_transparent {
        pixel = blend(destination, pixel, state.transparency);
    }

    if state.force_mask_bit || texel_mask_bit {
        pixel |= 0x8000;
    }

    vram.set(x, y, pixel);
}

/// Lê um texel da texture page corrente aplicando a texture window.
fn fetch_texel(vram: &Vram, state: &DrawState, u: u8, v: u8) -> u16 {
    // PSX-SPX: `coord = (coord AND NOT(mask*8)) OR ((offset AND mask)*8)`.
    let mask_x = state.texture_window_mask_x as u32;
    let mask_y = state.texture_window_mask_y as u32;
    let u =
        ((u as u32 & !(mask_x * 8)) | ((state.texture_window_offset_x as u32 & mask_x) * 8)) as i32;
    let v =
        ((v as u32 & !(mask_y * 8)) | ((state.texture_window_offset_y as u32 & mask_y) * 8)) as i32;

    match state.texture_depth {
        TextureDepth::Bpp4 => {
            let word = vram.get(state.texpage_x + (u >> 2), state.texpage_y + v);
            let index = (word >> ((u & 3) * 4)) & 0xF;
            vram.get(state.clut_x + index as i32, state.clut_y)
        }
        TextureDepth::Bpp8 => {
            let word = vram.get(state.texpage_x + (u >> 1), state.texpage_y + v);
            let index = (word >> ((u & 1) * 8)) & 0xFF;
            vram.get(state.clut_x + index as i32, state.clut_y)
        }
        TextureDepth::Bpp15 => vram.get(state.texpage_x + u, state.texpage_y + v),
    }
}

/// Expande BGR555 para 8 bits por canal.
#[inline]
const fn expand_bgr555(pixel: u16) -> Color {
    Color {
        r: ((pixel & 0x1F) << 3) as u8,
        g: (((pixel >> 5) & 0x1F) << 3) as u8,
        b: (((pixel >> 10) & 0x1F) << 3) as u8,
    }
}

/// Empacota 8 bits por canal em BGR555 (sem o mask bit).
#[inline]
const fn pack_bgr555(color: Color) -> u16 {
    ((color.r as u16) >> 3) | (((color.g as u16) >> 3) << 5) | (((color.b as u16) >> 3) << 10)
}

/// Multiplica o texel pela cor do vértice. `0x80` é o fator neutro.
#[inline]
fn modulate(texel: Color, vertex: Color) -> Color {
    let mix = |t: u8, c: u8| (((t as u32) * (c as u32)) >> 7).min(255) as u8;
    Color {
        r: mix(texel.r, vertex.r),
        g: mix(texel.g, vertex.g),
        b: mix(texel.b, vertex.b),
    }
}

/// Mistura o pixel novo (`front`) com o que já estava na VRAM (`back`).
fn blend(back: u16, front: u16, mode: TransparencyMode) -> u16 {
    let channel = |shift: u32| {
        let b = ((back >> shift) & 0x1F) as i32;
        let f = ((front >> shift) & 0x1F) as i32;
        let value = match mode {
            TransparencyMode::Half => (b + f) / 2,
            TransparencyMode::Add => b + f,
            TransparencyMode::Subtract => b - f,
            TransparencyMode::AddQuarter => b + f / 4,
        };
        value.clamp(0, 31) as u16
    };
    channel(0) | (channel(5) << 5) | (channel(10) << 10)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> DrawState {
        DrawState {
            area_right: 1023,
            area_bottom: 511,
            ..DrawState::default()
        }
    }

    fn vertex(x: i32, y: i32, color: Color) -> Vertex {
        Vertex {
            x,
            y,
            color,
            u: 0,
            v: 0,
        }
    }

    const RED: Color = Color { r: 255, g: 0, b: 0 };
    const WHITE: Color = Color {
        r: 255,
        g: 255,
        b: 255,
    };

    #[test]
    fn flat_triangle_fills_its_interior() {
        let mut vram = Vram::new();
        let s = state();
        draw_triangle(
            &mut vram,
            &s,
            &[vertex(0, 0, RED), vertex(16, 0, RED), vertex(0, 16, RED)],
            &Attributes::default(),
        );

        // Um ponto claramente dentro.
        assert_eq!(vram.get(2, 2), 0x001F, "interior pintado de vermelho");
        // Um ponto claramente fora da hipotenusa.
        assert_eq!(vram.get(15, 15), 0, "exterior intocado");
    }

    #[test]
    fn degenerate_triangle_draws_nothing() {
        let mut vram = Vram::new();
        draw_triangle(
            &mut vram,
            &state(),
            &[
                vertex(0, 0, WHITE),
                vertex(10, 0, WHITE),
                vertex(20, 0, WHITE),
            ],
            &Attributes::default(),
        );
        assert!(vram.as_slice().iter().all(|&p| p == 0));
    }

    #[test]
    fn oversized_triangle_is_rejected_like_hardware() {
        let mut vram = Vram::new();
        draw_triangle(
            &mut vram,
            &state(),
            &[
                vertex(0, 0, WHITE),
                vertex(1024, 0, WHITE),
                vertex(0, 10, WHITE),
            ],
            &Attributes::default(),
        );
        assert!(vram.as_slice().iter().all(|&p| p == 0));
    }

    #[test]
    fn drawing_area_clips_the_primitive() {
        let mut vram = Vram::new();
        let s = DrawState {
            area_left: 4,
            area_top: 4,
            area_right: 8,
            area_bottom: 8,
            ..state()
        };
        draw_triangle(
            &mut vram,
            &s,
            &[
                vertex(0, 0, WHITE),
                vertex(32, 0, WHITE),
                vertex(0, 32, WHITE),
            ],
            &Attributes::default(),
        );
        assert_eq!(vram.get(0, 0), 0, "acima/à esquerda da área foi cortado");
        assert_ne!(vram.get(5, 5), 0, "dentro da área foi pintado");
        assert_eq!(vram.get(9, 5), 0, "à direita da área foi cortado");
    }

    #[test]
    fn drawing_offset_shifts_the_primitive() {
        let mut vram = Vram::new();
        let s = DrawState {
            offset_x: 100,
            offset_y: 50,
            ..state()
        };
        draw_rectangle(
            &mut vram,
            &s,
            &vertex(0, 0, RED),
            (4, 4),
            &Attributes::default(),
        );
        assert_eq!(vram.get(0, 0), 0);
        assert_eq!(vram.get(100, 50), 0x001F);
        assert_eq!(vram.get(103, 53), 0x001F);
        assert_eq!(vram.get(104, 54), 0);
    }

    #[test]
    fn gouraud_shading_interpolates_between_vertices() {
        let mut vram = Vram::new();
        let s = state();
        let black = Color { r: 0, g: 0, b: 0 };
        draw_triangle(
            &mut vram,
            &s,
            &[
                vertex(0, 0, WHITE),
                vertex(63, 0, black),
                vertex(0, 63, black),
            ],
            &Attributes {
                shaded: true,
                ..Attributes::default()
            },
        );
        let near_white = vram.get(1, 1) & 0x1F;
        let near_black = vram.get(50, 1) & 0x1F;
        assert!(
            near_white > near_black,
            "gradiente do vértice branco para o preto: {near_white} vs {near_black}"
        );
    }

    #[test]
    fn mask_bit_protects_existing_pixels() {
        let mut vram = Vram::new();
        vram.set(2, 2, 0x8000 | 0x03E0); // verde com mask bit
        let s = DrawState {
            check_mask_bit: true,
            ..state()
        };
        draw_rectangle(
            &mut vram,
            &s,
            &vertex(0, 0, RED),
            (8, 8),
            &Attributes::default(),
        );
        assert_eq!(
            vram.get(2, 2),
            0x8000 | 0x03E0,
            "pixel mascarado preservado"
        );
        assert_eq!(vram.get(3, 3), 0x001F, "vizinho foi pintado");
    }

    #[test]
    fn force_mask_bit_sets_bit15() {
        let mut vram = Vram::new();
        let s = DrawState {
            force_mask_bit: true,
            ..state()
        };
        draw_rectangle(
            &mut vram,
            &s,
            &vertex(0, 0, RED),
            (2, 2),
            &Attributes::default(),
        );
        assert_eq!(vram.get(0, 0), 0x8000 | 0x001F);
    }

    #[test]
    fn semi_transparency_half_averages_with_background() {
        let mut vram = Vram::new();
        // Fundo: vermelho máximo (0x1F).
        for y in 0..4 {
            for x in 0..4 {
                vram.set(x, y, 0x001F);
            }
        }
        let s = state();
        draw_rectangle(
            &mut vram,
            &s,
            &vertex(0, 0, RED),
            (4, 4),
            &Attributes {
                semi_transparent: true,
                ..Attributes::default()
            },
        );
        // (31 + 31) / 2 = 31 — satura no mesmo valor.
        assert_eq!(vram.get(0, 0) & 0x1F, 31);

        // Agora com fundo preto: (0 + 31)/2 = 15.
        let mut vram = Vram::new();
        draw_rectangle(
            &mut vram,
            &s,
            &vertex(0, 0, RED),
            (4, 4),
            &Attributes {
                semi_transparent: true,
                ..Attributes::default()
            },
        );
        assert_eq!(vram.get(0, 0) & 0x1F, 15);
    }

    #[test]
    fn subtract_mode_clamps_at_zero() {
        assert_eq!(blend(0x0005, 0x000A, TransparencyMode::Subtract) & 0x1F, 0);
        assert_eq!(blend(0x000A, 0x0005, TransparencyMode::Subtract) & 0x1F, 5);
    }

    #[test]
    fn add_mode_clamps_at_31() {
        assert_eq!(blend(0x001F, 0x001F, TransparencyMode::Add) & 0x1F, 31);
    }

    #[test]
    fn textured_rectangle_samples_the_clut() {
        let mut vram = Vram::new();
        // CLUT em (0, 100): índice 1 = azul puro.
        vram.set(1, 100, 0x7C00);
        // Texture page em (0, 200): um word com nibbles todos = 1.
        for x in 0..4 {
            vram.set(x, 200, 0x1111);
        }

        let s = DrawState {
            texpage_x: 0,
            texpage_y: 200,
            clut_x: 0,
            clut_y: 100,
            texture_depth: TextureDepth::Bpp4,
            area_right: 1023,
            area_bottom: 511,
            ..DrawState::default()
        };

        draw_rectangle(
            &mut vram,
            &s,
            &Vertex {
                x: 300,
                y: 300,
                color: Color::NEUTRAL,
                u: 0,
                v: 0,
            },
            (4, 1),
            &Attributes {
                textured: true,
                raw_texture: true,
                ..Attributes::default()
            },
        );

        assert_eq!(vram.get(300, 300), 0x7C00, "texel azul amostrado da CLUT");
        assert_eq!(vram.get(303, 300), 0x7C00);
    }

    #[test]
    fn fully_transparent_texel_is_skipped() {
        let mut vram = Vram::new();
        // Fundo verde.
        vram.set(300, 300, 0x03E0);
        // CLUT com índice 0 = 0x0000 (transparente); texture page zerada.
        let s = DrawState {
            texpage_y: 200,
            clut_y: 100,
            area_right: 1023,
            area_bottom: 511,
            ..DrawState::default()
        };
        draw_rectangle(
            &mut vram,
            &s,
            &Vertex {
                x: 300,
                y: 300,
                color: Color::NEUTRAL,
                u: 0,
                v: 0,
            },
            (1, 1),
            &Attributes {
                textured: true,
                raw_texture: true,
                ..Attributes::default()
            },
        );
        assert_eq!(vram.get(300, 300), 0x03E0, "fundo preservado");
    }

    #[test]
    fn modulation_with_neutral_color_is_identity() {
        let texel = Color {
            r: 248,
            g: 128,
            b: 0,
        };
        assert_eq!(modulate(texel, Color::NEUTRAL), texel);
    }

    #[test]
    fn horizontal_line_is_drawn() {
        let mut vram = Vram::new();
        let s = state();
        draw_line(
            &mut vram,
            &s,
            &vertex(0, 5, RED),
            &vertex(10, 5, RED),
            false,
            false,
        );
        for x in 0..=10 {
            assert_eq!(vram.get(x, 5), 0x001F, "pixel {x} da linha");
        }
        assert_eq!(vram.get(11, 5), 0);
    }

    #[test]
    fn vertex_coordinates_are_eleven_bit_signed() {
        // 0x7FF = 2047, que como 11 bits com sinal é -1.
        let v = Vertex::from_command(0x0000_07FF);
        assert_eq!(v.x, -1);
        let v = Vertex::from_command(0x07FF_0000);
        assert_eq!(v.y, -1);
        let v = Vertex::from_command(0x0002_0003);
        assert_eq!((v.x, v.y), (3, 2));
    }

    #[test]
    fn dither_matrix_matches_the_hardware_pattern() {
        // Ponto de checagem contra a tabela do PSX-SPX.
        assert_eq!(DITHER[0][0], -4);
        assert_eq!(DITHER[0][3], 1);
        assert_eq!(DITHER[3][0], 3);
        assert_eq!(DITHER[3][3], -2);
    }
}
