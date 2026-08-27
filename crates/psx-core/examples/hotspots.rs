//! Onde o CPU está gastando o tempo.
//!
//! Quando um jogo carrega e depois congela, o que resolve é saber em que
//! endereço ele está girando. Este exemplo roda o console e amostra o `PC` a
//! cada frame, imprimindo os endereços mais frequentes.
//!
//! ```sh
//! cargo run -p psx-core --example hotspots -- --bios bios/SCPH1001.BIN \
//!     --disc games/xenogears/xenogears-disk-1.cue --frames 1800 --skip 600
//! ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use psx_core::{Bios, System};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut bios_path = String::from("bios/SCPH1001.BIN");
    let mut disc_path: Option<String> = None;
    let mut frames = 1800u32;
    // Frames descartados antes de começar a amostrar, para pular o boot.
    let mut skip = 0u32;
    // Endereço de RAM a observar frame a frame.
    let mut watch: Option<u32> = None;

    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let mut value = || {
            args.next()
                .ok_or_else(|| format!("{flag} precisa de um valor"))
        };
        match flag.as_str() {
            "--bios" => bios_path = value()?,
            "--disc" => disc_path = Some(value()?),
            "--frames" => frames = value()?.parse()?,
            "--skip" => skip = value()?.parse()?,
            "--watch" => {
                let text = value()?;
                let text = text.trim_start_matches("0x");
                watch = Some(u32::from_str_radix(text, 16)?);
            }
            other => return Err(format!("opção desconhecida: {other}").into()),
        }
    }

    let mut system = System::new(Bios::new(std::fs::read(&bios_path)?)?);
    if let Some(path) = &disc_path {
        load_disc(&mut system, Path::new(path))?;
    }

    let mut histogram: HashMap<u32, u32> = HashMap::new();
    // Valores distintos vistos no endereço observado, na ordem em que apareceram.
    let mut watched: Vec<(u32, u32)> = Vec::new();
    for frame in 0..frames {
        system.run_frame();
        if let Some(address) = watch {
            let value = system.bus().ram.read32(psx_core::memory::physical(address));
            if watched.last().map(|&(_, v)| v) != Some(value) {
                watched.push((frame, value));
            }
        }
        if frame >= skip {
            *histogram.entry(system.cpu().pc()).or_default() += 1;
        }
    }

    if let Some(address) = watch {
        println!("mudanças em {address:#010X}: {}", watched.len());
        for &(frame, value) in watched.iter().take(12) {
            println!("  frame {frame:5}: {value:#010X}");
        }
        if watched.len() > 12 {
            println!("  ... e mais {}", watched.len() - 12);
        }
        println!();
    }

    let mut entries: Vec<(u32, u32)> = histogram.into_iter().collect();
    entries.sort_by_key(|&(_, count)| std::cmp::Reverse(count));

    let sampled = frames.saturating_sub(skip);
    println!("amostras: {sampled} (uma por frame, a partir do frame {skip})");
    println!("endereços distintos: {}", entries.len());
    for (pc, count) in entries.iter().take(15) {
        println!(
            "  {pc:#010X}  {count:5}  {:.1}%",
            *count as f64 / sampled as f64 * 100.0
        );
    }

    // As palavras em volta do endereço mais quente: é o laço que trava.
    if let Some(&(hot, _)) = entries.first() {
        println!("\ninstruções em volta de {hot:#010X}:");
        let base = hot.wrapping_sub(16);
        for offset in 0..10u32 {
            let address = base.wrapping_add(offset * 4);
            let word = system.bus().ram.read32(psx_core::memory::physical(address));
            let marker = if address == hot { "  <== aqui" } else { "" };
            println!("  {address:#010X}  {word:#010X}  {}{marker}", decode(word));
        }
    }

    println!("\nestado final:");
    println!(
        "  I_STAT={:#06X} I_MASK={:#06X} pendente={}",
        system.bus().irq.stat(),
        system.bus().irq.mask(),
        system.bus().irq.is_pending()
    );
    println!("  cdrom: {}", system.bus().cdrom.debug_state());
    println!("  {:?}", system.bus().gpu.display_state());
    let d = system.diagnostics();
    println!(
        "  gte={} gpu={} cdrom={} leituras={} escritas={}",
        d.gte_unimplemented,
        d.gpu_unhandled,
        d.cdrom_unimplemented,
        d.bus_unhandled_reads,
        d.bus_unhandled_writes
    );
    Ok(())
}

/// Desmontagem mínima: só o suficiente para reconhecer um laço de espera.
fn decode(word: u32) -> String {
    let op = word >> 26;
    let rs = (word >> 21) & 0x1F;
    let rt = (word >> 16) & 0x1F;
    let rd = (word >> 11) & 0x1F;
    let imm = word as u16 as i16;
    let funct = word & 0x3F;
    match op {
        0x00 => match funct {
            _ if word == 0 => "nop".into(),
            0x08 => format!("jr     r{rs}"),
            0x09 => format!("jalr   r{rd}, r{rs}"),
            0x21 => format!("addu   r{rd}, r{rs}, r{rt}"),
            0x24 => format!("and    r{rd}, r{rs}, r{rt}"),
            0x25 => format!("or     r{rd}, r{rs}, r{rt}"),
            _ => format!("special {funct:#04X}"),
        },
        0x02 => format!("j      {:#010X}", (word & 0x03FF_FFFF) << 2),
        0x03 => format!("jal    {:#010X}", (word & 0x03FF_FFFF) << 2),
        0x04 => format!("beq    r{rs}, r{rt}, {imm:+}"),
        0x05 => format!("bne    r{rs}, r{rt}, {imm:+}"),
        0x08 => format!("addi   r{rt}, r{rs}, {imm}"),
        0x09 => format!("addiu  r{rt}, r{rs}, {imm}"),
        0x0C => format!("andi   r{rt}, r{rs}, {:#06X}", word as u16),
        0x0F => format!("lui    r{rt}, {:#06X}", word as u16),
        0x20 => format!("lb     r{rt}, {imm} off r{rs}"),
        0x23 => format!("lw     r{rt}, {imm} off r{rs}"),
        0x24 => format!("lbu    r{rt}, {imm} off r{rs}"),
        0x28 => format!("sb     r{rt}, {imm} off r{rs}"),
        0x2B => format!("sw     r{rt}, {imm} off r{rs}"),
        _ => format!("op {op:#04X}"),
    }
}

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
