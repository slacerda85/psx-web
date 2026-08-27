//! VRAM da GPU: 1 MB organizado como 1024×512 pixels de 16 bits.
//!
//! Referência: PSX-SPX — "GPU Memory Transfer Commands".

/// Largura da VRAM em pixels de 16 bits.
pub const VRAM_WIDTH: usize = 1024;
/// Altura da VRAM em linhas.
pub const VRAM_HEIGHT: usize = 512;

/// Framebuffer da GPU.
#[derive(Clone)]
pub struct Vram {
    pixels: Box<[u16]>,
}

impl Vram {
    pub fn new() -> Self {
        Self {
            pixels: vec![0; VRAM_WIDTH * VRAM_HEIGHT].into_boxed_slice(),
        }
    }

    /// Lê um pixel. As coordenadas dão wrap, como no hardware.
    #[inline(always)]
    pub fn get(&self, x: i32, y: i32) -> u16 {
        let x = (x as usize) & (VRAM_WIDTH - 1);
        let y = (y as usize) & (VRAM_HEIGHT - 1);
        self.pixels[y * VRAM_WIDTH + x]
    }

    /// Escreve um pixel. As coordenadas dão wrap, como no hardware.
    #[inline(always)]
    pub fn set(&mut self, x: i32, y: i32, value: u16) {
        let x = (x as usize) & (VRAM_WIDTH - 1);
        let y = (y as usize) & (VRAM_HEIGHT - 1);
        self.pixels[y * VRAM_WIDTH + x] = value;
    }

    pub fn as_slice(&self) -> &[u16] {
        &self.pixels
    }

    pub fn as_mut_slice(&mut self) -> &mut [u16] {
        &mut self.pixels
    }
}

impl Default for Vram {
    fn default() -> Self {
        Self::new()
    }
}

/// Converte um pixel `BGR555` da VRAM para `RGBA8` (alfa sempre opaco).
///
/// PSX-SPX: o bit 15 é o *mask bit*, não faz parte da cor. A expansão de 5
/// para 8 bits replica os bits altos (`x << 3 | x >> 2`), que é o que o
/// hardware de vídeo produz.
#[inline(always)]
pub const fn bgr555_to_rgba8(pixel: u16) -> [u8; 4] {
    let r = (pixel & 0x1F) as u8;
    let g = ((pixel >> 5) & 0x1F) as u8;
    let b = ((pixel >> 10) & 0x1F) as u8;
    [
        (r << 3) | (r >> 2),
        (g << 3) | (g >> 2),
        (b << 3) | (b >> 2),
        0xFF,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coordinates_wrap_instead_of_panicking() {
        let mut vram = Vram::new();
        vram.set(0, 0, 0x1234);
        assert_eq!(vram.get(VRAM_WIDTH as i32, VRAM_HEIGHT as i32), 0x1234);
        assert_eq!(vram.get(-(VRAM_WIDTH as i32), 0), 0x1234);
    }

    #[test]
    fn color_conversion_saturates_to_full_range() {
        assert_eq!(bgr555_to_rgba8(0x0000), [0, 0, 0, 0xFF]);
        // Branco: 5 bits ligados em cada canal.
        assert_eq!(bgr555_to_rgba8(0x7FFF), [0xFF, 0xFF, 0xFF, 0xFF]);
        // Vermelho puro fica nos bits 0..4.
        assert_eq!(bgr555_to_rgba8(0x001F), [0xFF, 0, 0, 0xFF]);
        // Azul puro nos bits 10..14.
        assert_eq!(bgr555_to_rgba8(0x7C00), [0, 0, 0xFF, 0xFF]);
    }

    #[test]
    fn mask_bit_does_not_leak_into_color() {
        assert_eq!(bgr555_to_rgba8(0x8000), [0, 0, 0, 0xFF]);
    }
}
