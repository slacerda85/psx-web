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
use std::path::{Path, PathBuf};

use psx_core::{Bios, System};

struct Options {
    bios: String,
    disc: Option<String>,
    frames: u32,
    output: String,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let options = parse_args()?;

    let bios = Bios::new(std::fs::read(&options.bios)?)?;
    let mut system = System::new(bios);

    if let Some(path) = &options.disc {
        load_disc(&mut system, Path::new(path))?;
        let disc = system.disc().expect("acabou de ser inserido");
        println!(
            "disco   : {:?}, {} setores, {} faixa(s)",
            disc.region(),
            disc.total_sectors(),
            disc.tracks().len()
        );
    }

    for _ in 0..options.frames {
        system.run_frame();
    }

    let width = system.frame_width();
    let height = system.frame_height();
    let pixels = system.framebuffer();

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

fn parse_args() -> Result<Options, Box<dyn std::error::Error>> {
    let mut options = Options {
        bios: "bios/SCPH1001.BIN".into(),
        disc: None,
        frames: 300,
        output: "screenshot.bmp".into(),
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
            other => return Err(format!("opção desconhecida: {other}").into()),
        }
    }
    Ok(options)
}

/// Insere um disco, aceitando tanto `.cue` quanto imagem crua.
fn load_disc(system: &mut System, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    if path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("cue"))
    {
        let cue = std::fs::read_to_string(path)?;
        let binary = locate_binary(path, &cue)?;
        system.load_disc_with_cue(&cue, std::fs::read(binary)?)?;
    } else {
        system.load_disc(std::fs::read(path)?)?;
    }
    Ok(())
}

/// Acha o binário de um CUE, tolerando que ele tenha sido renomeado.
fn locate_binary(cue_path: &Path, cue: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let directory = cue_path.parent().unwrap_or(Path::new("."));

    if let Some(declared) = cue
        .lines()
        .find(|line| line.trim_start().to_ascii_uppercase().starts_with("FILE"))
        .and_then(|line| line.split('"').nth(1))
    {
        let candidate = directory.join(declared);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }

    // Um .bin de jogo é sempre o maior arquivo da pasta.
    let mut candidates: Vec<PathBuf> = std::fs::read_dir(directory)?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && !path
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("cue"))
        })
        .collect();
    candidates.sort_by_key(|path| std::cmp::Reverse(path.metadata().map(|m| m.len()).unwrap_or(0)));

    candidates
        .into_iter()
        .next()
        .ok_or_else(|| format!("nenhum binário ao lado de {}", cue_path.display()).into())
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
