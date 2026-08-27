//! O que o jogo pediu ao hardware antes de parar.
//!
//! Um laço de espera é sempre o mesmo punhado de registradores repetindo. Este
//! exemplo roda o console, guarda os últimos acessos ao bloco de I/O e os
//! imprime com o nome do registrador — ver quais são e o que devolvemos costuma
//! apontar a causa sem precisar de um emulador de referência.
//!
//! ```sh
//! cargo run --release -p psx-core --example iotrace -- \
//!     --bios bios/SCPH1001.BIN --disc games/xenogears/xenogears-disk-1.cue \
//!     --frames 1500 --tail 60
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use psx_core::bus::AccessKind;
use psx_core::{Bios, System};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut bios_path = String::from("bios/SCPH1001.BIN");
    let mut disc_path: Option<String> = None;
    let mut frames = 1500u32;
    let mut tail = 60usize;
    let mut capacity = 4096usize;

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
            "--tail" => tail = value()?.parse()?,
            "--capacity" => capacity = value()?.parse()?,
            other => return Err(format!("opção desconhecida: {other}").into()),
        }
    }

    let mut system = System::new(Bios::new(std::fs::read(&bios_path)?)?);
    if let Some(path) = &disc_path {
        load_disc(&mut system, Path::new(path))?;
    }
    system.start_io_trace(capacity);

    for _ in 0..frames {
        system.run_frame();
    }

    let trace = system.bus().io_trace();
    println!("acessos guardados: {}\n", trace.len());

    // Quem repete mais é quem está no laço.
    let mut by_register: BTreeMap<u32, (u32, u32)> = BTreeMap::new();
    for access in &trace {
        let entry = by_register.entry(access.offset).or_insert((0, 0));
        match access.kind {
            AccessKind::Read => entry.0 += 1,
            AccessKind::Write => entry.1 += 1,
        }
    }

    println!("registradores no laço (leituras / escritas):");
    let mut ranked: Vec<_> = by_register.into_iter().collect();
    ranked.sort_by_key(|&(_, (reads, writes))| std::cmp::Reverse(reads + writes));
    for (offset, (reads, writes)) in ranked.iter().take(12) {
        println!(
            "  {:<22} {:>6} / {:<6}",
            register_name(*offset),
            reads,
            writes
        );
    }

    println!("\núltimos {tail} acessos:");
    for access in trace.iter().rev().take(tail).rev() {
        let arrow = match access.kind {
            AccessKind::Read => "->",
            AccessKind::Write => "<-",
        };
        println!(
            "  pc={:08X}  {:<22} {}{} {:0width$X}",
            access.pc,
            register_name(access.offset),
            arrow,
            if access.width == 4 { "" } else { "." },
            access.value,
            width = (access.width as usize) * 2,
        );
    }

    Ok(())
}

/// Nome do registrador a partir do offset dentro de `0x1F80_1000`.
fn register_name(offset: u32) -> String {
    let name = match offset {
        0x040 => "JOY_DATA",
        0x044 => "JOY_STAT",
        0x048 => "JOY_MODE",
        0x04A => "JOY_CTRL",
        0x04E => "JOY_BAUD",
        0x070 => "I_STAT",
        0x074 => "I_MASK",
        0x0F0 => "DPCR",
        0x0F4 => "DICR",
        0x100..=0x12F => "TIMER",
        0x800 => "CDROM_STAT",
        0x801 => "CDROM_CMD/RESP",
        0x802 => "CDROM_DATA",
        0x803 => "CDROM_IRQ",
        0x810 => "GP0/GPUREAD",
        0x814 => "GP1/GPUSTAT",
        0x820 => "MDEC_CMD/DATA",
        0x824 => "MDEC_CTRL/STAT",
        0x080..=0x0EF => {
            let channel = (offset - 0x080) / 0x10;
            let which = match offset & 0x0F {
                0x00 => "MADR",
                0x04 => "BCR",
                _ => "CHCR",
            };
            return format!("DMA{channel}_{which}");
        }
        0xC00..=0xFFF => "SPU",
        _ => return format!("{:#06X}", 0x1F80_1000 + offset),
    };
    name.to_string()
}

fn load_disc(system: &mut System, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    if path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("cue"))
    {
        let cue = std::fs::read_to_string(path)?;
        let directory = path.parent().unwrap_or(Path::new("."));
        let mut candidates: Vec<PathBuf> = std::fs::read_dir(directory)?
            .flatten()
            .map(|entry| entry.path())
            .filter(|candidate| {
                candidate.is_file()
                    && !candidate
                        .extension()
                        .is_some_and(|extension| extension.eq_ignore_ascii_case("cue"))
            })
            .collect();
        candidates.sort_by_key(|candidate| {
            std::cmp::Reverse(candidate.metadata().map(|meta| meta.len()).unwrap_or(0))
        });
        let binary = candidates
            .into_iter()
            .next()
            .ok_or_else(|| format!("nenhum binário ao lado de {}", path.display()))?;
        system.load_disc_with_cue(&cue, std::fs::read(binary)?)?;
    } else {
        system.load_disc(std::fs::read(path)?)?;
    }
    Ok(())
}
