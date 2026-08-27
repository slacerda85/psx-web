//! Ferramenta de depuração visual: roda a BIOS por N frames e grava o
//! framebuffer em BMP.
//!
//! Ver o que a GPU produziu é a forma mais rápida de diagnosticar o
//! rasterizador — um teste unitário diz que um triângulo tem os pixels
//! certos, mas não que a tela inteira faz sentido.
//!
//! ```sh
//! cargo run -p psx-core --example screenshot -- bios/SCPH1001.BIN 300 boot.bmp
//! ```
//!
//! BMP é escrito à mão de propósito: o core não tem dependências e não vai
//! ganhar uma só para salvar imagem de debug.

use std::io::Write;

use psx_core::{Bios, System};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let bios_path = args.next().unwrap_or_else(|| "bios/SCPH1001.BIN".into());
    let frames: u32 = args.next().unwrap_or_else(|| "300".into()).parse()?;
    let output = args.next().unwrap_or_else(|| "screenshot.bmp".into());

    let bytes = std::fs::read(&bios_path)
        .map_err(|error| format!("não consegui ler {bios_path}: {error}"))?;
    let bios = Bios::new(bytes)?;
    let mut system = System::new(bios);

    for _ in 0..frames {
        system.run_frame();
    }

    let width = system.frame_width();
    let height = system.frame_height();
    let pixels = system.framebuffer();

    let non_black = count_non_black(pixels, width, height);
    let total = (width * height) as u64;
    let diagnostics = system.diagnostics();

    println!("frames executados : {frames}");
    println!("resolução         : {width}x{height}");
    println!(
        "pixels não-pretos : {non_black} de {total} ({:.1}%)",
        non_black as f64 / total as f64 * 100.0
    );
    println!(
        "diagnóstico       : gte={} gpu={} cdrom={} leituras={} escritas={}",
        diagnostics.gte_unimplemented,
        diagnostics.gpu_unhandled,
        diagnostics.cdrom_unimplemented,
        diagnostics.bus_unhandled_reads,
        diagnostics.bus_unhandled_writes,
    );

    write_bmp(&output, pixels, width, height)?;
    println!("gravado           : {output}");
    Ok(())
}

fn count_non_black(pixels: &[u8], width: u32, height: u32) -> u64 {
    let mut count = 0;
    for index in 0..(width * height) as usize {
        let offset = index * 4;
        // Alpha é ignorado: só interessa se algo foi desenhado.
        if pixels[offset] != 0 || pixels[offset + 1] != 0 || pixels[offset + 2] != 0 {
            count += 1;
        }
    }
    count
}

/// Grava um BMP de 24 bits sem compressão.
fn write_bmp(
    path: &str,
    pixels: &[u8],
    width: u32,
    height: u32,
) -> Result<(), Box<dyn std::error::Error>> {
    // Cada linha do BMP é alinhada em 4 bytes.
    let row_padding = (4 - (width as usize * 3) % 4) % 4;
    let row_size = width as usize * 3 + row_padding;
    let pixel_bytes = row_size * height as usize;
    const HEADER_SIZE: u32 = 14 + 40;

    let mut file = std::io::BufWriter::new(std::fs::File::create(path)?);

    file.write_all(b"BM")?;
    file.write_all(&(HEADER_SIZE + pixel_bytes as u32).to_le_bytes())?;
    file.write_all(&0u32.to_le_bytes())?; // reservado
    file.write_all(&HEADER_SIZE.to_le_bytes())?;

    file.write_all(&40u32.to_le_bytes())?; // tamanho do DIB header
    file.write_all(&(width as i32).to_le_bytes())?;
    file.write_all(&(height as i32).to_le_bytes())?;
    file.write_all(&1u16.to_le_bytes())?; // planos
    file.write_all(&24u16.to_le_bytes())?; // bits por pixel
    file.write_all(&0u32.to_le_bytes())?; // sem compressão
    file.write_all(&(pixel_bytes as u32).to_le_bytes())?;
    file.write_all(&2835i32.to_le_bytes())?; // 72 DPI horizontal
    file.write_all(&2835i32.to_le_bytes())?; // 72 DPI vertical
    file.write_all(&0u32.to_le_bytes())?; // cores na paleta
    file.write_all(&0u32.to_le_bytes())?; // cores importantes

    // BMP guarda as linhas de baixo para cima e os canais em BGR.
    let padding = vec![0u8; row_padding];
    for y in (0..height).rev() {
        for x in 0..width {
            let offset = ((y * width + x) * 4) as usize;
            file.write_all(&[pixels[offset + 2], pixels[offset + 1], pixels[offset]])?;
        }
        file.write_all(&padding)?;
    }

    file.flush()?;
    Ok(())
}
