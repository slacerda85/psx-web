//! Ferramenta de depuração visual: roda o console por N frames e grava o
//! framebuffer em BMP.
//!
//! Ver o que a GPU produziu é a forma mais rápida de diagnosticar o
//! rasterizador — um teste unitário diz que um triângulo tem os pixels
//! certos, mas não que a tela inteira faz sentido.
//!
//! ```sh
//! cargo run -p psx-core --example screenshot -- --bios bios/SCPH1001.BIN --frames 300
//! cargo run -p psx-core --example screenshot -- --bios bios/SCPH1001.BIN \
//!     --disc games/xenogears/xenogears-disk-1.cue --frames 1800 --out xeno.bmp
//! ```
//!
//! BMP é escrito à mão de propósito: o core não tem dependências e não vai
//! ganhar uma só para salvar imagem de debug.

use std::io::Write;
use std::path::Path;

#[path = "common/disc.rs"]
mod disc_loader;

use psx_core::sio::ButtonState;
use psx_core::{Bios, System};

struct Options {
    bios: String,
    disc: Option<String>,
    frames: u32,
    output: String,
    /// Grava a VRAM inteira (1024x512) em vez da janela de display.
    vram: bool,
    /// Botão mantido pressionado durante a segunda metade da execução.
    press: Option<String>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let options = parse_args()?;

    let bios = Bios::new(std::fs::read(&options.bios)?)?;
    let mut system = System::new(bios);

    if let Some(path) = &options.disc {
        disc_loader::load(&mut system, Path::new(path))?;
        let disc = system.disc().expect("acabou de ser inserido");
        println!(
            "disco   : {:?}, {} setores, {} faixa(s)",
            disc.region(),
            disc.total_sectors(),
            disc.tracks().len()
        );
    }

    // Um botão segurado na segunda metade da execução: é o que mostra se o
    // console reage à entrada, separando "o core ignora os botões" de "o
    // navegador não entrega as teclas".
    let held = match options.press.as_deref() {
        None => 0u16,
        Some(name) => 1 << button_bit(name)?,
    };

    for frame in 0..options.frames {
        if held != 0 && frame >= options.frames / 2 {
            system.set_buttons(0, ButtonState::from_pressed_mask(held));
        }
        system.run_frame();
    }

    // A VRAM inteira separa dois diagnósticos que a janela de display confunde:
    // "a GPU não desenhou nada" e "desenhou fora da área que está sendo exibida".
    println!(
        "irq     : I_MASK={:#06X} I_STAT={:#06X}",
        system.bus().irq.mask(),
        system.bus().irq.stat()
    );
    println!("sio     : {}", system.bus().sio.debug_state());
    println!("display : {:?}", system.bus().gpu.display_state());
    let vram = system.bus().gpu.vram().as_slice();
    let vram_used = vram.iter().filter(|&&pixel| pixel != 0).count();
    println!(
        "vram    : {vram_used} de {} halfwords não-zero ({:.1}%)",
        vram.len(),
        vram_used as f64 / vram.len() as f64 * 100.0
    );

    let (width, height, owned);
    let pixels: &[u8] = if options.vram {
        width = psx_core::gpu::VRAM_WIDTH as u32;
        height = psx_core::gpu::VRAM_HEIGHT as u32;
        owned = vram
            .iter()
            .flat_map(|&pixel| {
                let r = ((pixel & 0x1F) << 3) as u8;
                let g = (((pixel >> 5) & 0x1F) << 3) as u8;
                let b = (((pixel >> 10) & 0x1F) << 3) as u8;
                [r, g, b, 0xFF]
            })
            .collect::<Vec<u8>>();
        &owned
    } else {
        width = system.frame_width();
        height = system.frame_height();
        system.framebuffer()
    };

    let non_black = count_non_black(pixels, width, height);
    let total = (width * height) as u64;
    let diagnostics = system.diagnostics();

    println!("frames  : {}", options.frames);
    println!("resolução: {width}x{height}");
    println!(
        "desenhado: {non_black} de {total} pixels ({:.1}%)",
        non_black as f64 / total as f64 * 100.0
    );
    println!(
        "diagnóstico: gte={} gpu={} cdrom={} leituras={} escritas={}",
        diagnostics.gte_unimplemented,
        diagnostics.gpu_unhandled,
        diagnostics.cdrom_unimplemented,
        diagnostics.bus_unhandled_reads,
        diagnostics.bus_unhandled_writes,
    );

    if diagnostics.cdrom_unimplemented > 0 {
        println!(
            "cdrom   : último comando sem implementação = {:#04X}",
            system.bus().cdrom.last_unimplemented_command()
        );
    }
    if diagnostics.gte_unimplemented > 0 {
        println!(
            "gte     : último comando sem implementação = {:#04X}",
            system.cpu().gte.last_unimplemented_command()
        );
    }

    write_bmp(&options.output, pixels, width, height)?;
    println!("gravado : {}", options.output);
    Ok(())
}

/// Número do bit de um botão, na ordem do protocolo SIO0.
fn button_bit(name: &str) -> Result<u16, Box<dyn std::error::Error>> {
    let bit = match name.to_ascii_lowercase().as_str() {
        "select" => 0,
        "l3" => 1,
        "r3" => 2,
        "start" => 3,
        "up" => 4,
        "right" => 5,
        "down" => 6,
        "left" => 7,
        "l2" => 8,
        "r2" => 9,
        "l1" => 10,
        "r1" => 11,
        "triangle" => 12,
        "circle" => 13,
        "cross" => 14,
        "square" => 15,
        other => return Err(format!("botão desconhecido: {other}").into()),
    };
    Ok(bit)
}

fn parse_args() -> Result<Options, Box<dyn std::error::Error>> {
    let mut options = Options {
        bios: "bios/SCPH1001.BIN".into(),
        disc: None,
        frames: 300,
        output: "screenshot.bmp".into(),
        vram: false,
        press: None,
    };

    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let mut value = || {
            args.next()
                .ok_or_else(|| format!("{flag} precisa de um valor"))
        };
        match flag.as_str() {
            "--bios" => options.bios = value()?,
            "--disc" => options.disc = Some(value()?),
            "--frames" => options.frames = value()?.parse()?,
            "--out" => options.output = value()?,
            "--vram" => options.vram = true,
            "--press" => options.press = Some(value()?),
            other => return Err(format!("opção desconhecida: {other}").into()),
        }
    }
    Ok(options)
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
