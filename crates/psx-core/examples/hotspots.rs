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
use std::path::Path;

#[path = "common/disc.rs"]
mod disc_loader;

use psx_core::{Bios, System};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut bios_path = String::from("bios/SCPH1001.BIN");
    let mut disc_path: Option<String> = None;
    let mut frames = 1800u32;
    // Frames descartados antes de começar a amostrar, para pular o boot.
    let mut skip = 0u32;
    // Endereço de RAM a observar frame a frame.
    let mut watch: Option<u32> = None;
    let mut dumps: Vec<(u32, u32)> = Vec::new();

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
            "--dump" => {
                let text = value()?;
                let (addr, count) = text.split_once(":").unwrap_or((text.as_str(), "16"));
                dumps.push((
                    u32::from_str_radix(addr.trim_start_matches("0x"), 16)?,
                    count.parse()?,
                ));
            }
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
        disc_loader::load(&mut system, Path::new(path))?;
    }
    if let Some(address) = watch {
        // O rastro de I/O é quem guarda o PC corrente; ligá-lo com capacidade
        // mínima basta para o watchpoint saber quem escreveu.
        system.start_io_trace(1);
        system.bus_mut().watch_ram(address, 64);
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
        let writes = system.bus().ram_watch();
        println!("escritas em {address:#010X}: {}", writes.len());
        for write in writes.iter().take(20) {
            let kind = if write.width == 0 {
                "dma".to_string()
            } else {
                format!("st{}", write.width * 8)
            };
            println!(
                "  pc={:08X} {kind:>5} {:08X} <- {:08X}",
                write.pc, write.address, write.value
            );
        }
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

    for &(base, count) in &dumps {
        println!("\n--- {base:#010X} ({count} palavras) ---");
        for offset in 0..count {
            let address = base.wrapping_add(offset * 4);
            let word = system.bus().ram.read32(psx_core::memory::physical(address));
            println!("  {address:#010X}  {word:#010X}  {}", decode(word));
        }
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

    // O que o kernel imprimiu: um "VSync: timeout" ou um erro de boot diz
    // mais em uma linha do que o histograma inteiro.
    let tty = system.tty();
    if !tty.is_empty() {
        println!(
            "
últimas linhas da TTY:"
        );
        for line in tty.lines().rev().take(12).collect::<Vec<_>>().iter().rev() {
            println!("  {line}");
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
            0x00 => format!("sll    r{rd}, r{rt}, {}", (word >> 6) & 0x1F),
            0x02 => format!("srl    r{rd}, r{rt}, {}", (word >> 6) & 0x1F),
            0x03 => format!("sra    r{rd}, r{rt}, {}", (word >> 6) & 0x1F),
            0x08 => format!("jr     r{rs}"),
            0x09 => format!("jalr   r{rd}, r{rs}"),
            0x0C => "syscall".into(),
            0x20 => format!("add    r{rd}, r{rs}, r{rt}"),
            0x21 => format!("addu   r{rd}, r{rs}, r{rt}"),
            0x23 => format!("subu   r{rd}, r{rs}, r{rt}"),
            0x24 => format!("and    r{rd}, r{rs}, r{rt}"),
            0x25 => format!("or     r{rd}, r{rs}, r{rt}"),
            0x2A => format!("slt    r{rd}, r{rs}, r{rt}"),
            0x2B => format!("sltu   r{rd}, r{rs}, r{rt}"),
            _ => format!("special {funct:#04X}"),
        },
        0x01 => match rt {
            0x00 => format!("bltz   r{rs}, {imm:+}"),
            0x01 => format!("bgez   r{rs}, {imm:+}"),
            _ => format!("regimm {rt:#04X}"),
        },
        0x02 => format!("j      {:#010X}", (word & 0x03FF_FFFF) << 2),
        0x03 => format!("jal    {:#010X}", (word & 0x03FF_FFFF) << 2),
        0x04 => format!("beq    r{rs}, r{rt}, {imm:+}"),
        0x05 => format!("bne    r{rs}, r{rt}, {imm:+}"),
        0x06 => format!("blez   r{rs}, {imm:+}"),
        0x07 => format!("bgtz   r{rs}, {imm:+}"),
        0x08 => format!("addi   r{rt}, r{rs}, {imm}"),
        0x09 => format!("addiu  r{rt}, r{rs}, {imm}"),
        0x0A => format!("slti   r{rt}, r{rs}, {imm}"),
        0x0B => format!("sltiu  r{rt}, r{rs}, {imm}"),
        0x0C => format!("andi   r{rt}, r{rs}, {:#06X}", word as u16),
        0x0D => format!("ori    r{rt}, r{rs}, {:#06X}", word as u16),
        0x0F => format!("lui    r{rt}, {:#06X}", word as u16),
        0x10 => format!("cop0   {:#010X}", word & 0x03FF_FFFF),
        0x12 => format!("cop2   {:#010X}", word & 0x01FF_FFFF),
        0x20 => format!("lb     r{rt}, {imm} off r{rs}"),
        0x21 => format!("lh     r{rt}, {imm} off r{rs}"),
        0x23 => format!("lw     r{rt}, {imm} off r{rs}"),
        0x24 => format!("lbu    r{rt}, {imm} off r{rs}"),
        0x25 => format!("lhu    r{rt}, {imm} off r{rs}"),
        0x28 => format!("sb     r{rt}, {imm} off r{rs}"),
        0x29 => format!("sh     r{rt}, {imm} off r{rs}"),
        0x2B => format!("sw     r{rt}, {imm} off r{rs}"),
        _ => format!("op {op:#04X}"),
    }
}
