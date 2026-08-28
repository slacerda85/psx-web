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
use std::path::Path;

#[path = "common/disc.rs"]
mod disc_loader;

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
        disc_loader::load(&mut system, Path::new(path))?;
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
        // Dentro da SPU o offset importa: 24 leituras seguidas de "SPU" não
        // dizem nada, mas "SPU voz 3 ADSRVOL" diz tudo.
        0xC00..=0xFFF => {
            let inner = offset - 0xC00;
            if inner < 0x180 {
                let voice = inner / 0x10;
                let which = match (inner % 0x10) / 2 {
                    0 => "VOLL",
                    1 => "VOLR",
                    2 => "PITCH",
                    3 => "ADDR",
                    4 => "ADSR_LO",
                    5 => "ADSR_HI",
                    6 => "ADSRVOL",
                    _ => "REPEAT",
                };
                return format!("SPU v{voice:02}.{which}");
            }
            let which = match inner {
                0x180 | 0x182 => "MVOL",
                0x188 | 0x18A => "KON",
                0x18C | 0x18E => "KOFF",
                0x190 => "PMON",
                0x194 => "NON",
                0x198 => "EON",
                0x19C | 0x19E => "ENDX",
                0x1A4 => "IRQ_ADDR",
                0x1A6 => "XFER_ADDR",
                0x1A8 => "XFER_FIFO",
                0x1AA => "SPUCNT",
                0x1AE => "SPUSTAT",
                0x1B0 | 0x1B2 => "CD_VOL",
                _ => return format!("SPU+{inner:03X}"),
            };
            return format!("SPU {which}");
        }
        _ => return format!("{:#06X}", 0x1F80_1000 + offset),
    };
    name.to_string()
}
