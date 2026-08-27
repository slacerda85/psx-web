//! Boot contra uma BIOS real.
//!
//! Estes testes só rodam se houver uma BIOS em `bios/` na raiz do repositório.
//! O projeto não distribui BIOS e o `.gitignore` bloqueia a pasta inteira, então
//! no CI eles são **pulados**, não falham. Rodar localmente com a sua própria
//! BIOS é o que dá confiança de que CPU, bus e kernel estão de pé — nenhum
//! teste unitário substitui executar o código real da Sony.

use std::path::PathBuf;

use psx_core::{Bios, System};

/// Teto de ciclos para chegar ao shell: ~15 segundos de console.
const BOOT_BUDGET: u64 = 500_000_000;

/// Procura uma BIOS na pasta `bios/` da raiz do workspace.
fn find_bios() -> Option<(PathBuf, Vec<u8>)> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../bios");
    let entries = std::fs::read_dir(root).ok()?;

    let mut candidates: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .collect();
    // Ordem estável: o resultado não pode depender da ordem do sistema de
    // arquivos, senão o teste vira loteria entre SCPH1001 e SCPH1002.
    candidates.sort();

    for path in candidates {
        let bytes = std::fs::read(&path).ok()?;
        if bytes.len() == 512 * 1024 {
            return Some((path, bytes));
        }
    }
    None
}

macro_rules! bios_or_skip {
    () => {
        match find_bios() {
            Some(found) => found,
            None => {
                eprintln!("pulado: nenhuma BIOS de 512 KB em bios/");
                return;
            }
        }
    };
}

#[test]
fn a_bios_real_e_aceita_e_o_console_reseta() {
    let (path, bytes) = bios_or_skip!();

    let bios = Bios::new(bytes).expect("BIOS de 512 KB deve ser aceita");
    let mut system = System::new(bios);
    system.reset();

    // Após o reset o PC tem que estar no vetor de boot da BIOS.
    assert_eq!(
        system.cpu().pc(),
        0xBFC0_0000,
        "reset com {} deve entrar pelo vetor de boot",
        path.display()
    );
}

#[test]
fn a_bios_real_executa_ate_o_shell() {
    let (path, bytes) = bios_or_skip!();

    let bios = Bios::new(bytes).expect("BIOS de 512 KB deve ser aceita");
    let mut system = System::new(bios);

    let reached = system.run_until_shell(BOOT_BUDGET);
    let diagnostics = system.diagnostics();

    // Os contadores contam a história mesmo quando o boot funciona: qualquer
    // um deles diferente de zero é funcionalidade que o kernel tocou e que o
    // emulador ainda não implementa.
    eprintln!(
        "BIOS {}: shell={} pc=0x{:08X} gte={} gpu={} cdrom={} leituras={} escritas={}",
        path.display(),
        reached,
        system.cpu().pc(),
        diagnostics.gte_unimplemented,
        diagnostics.gpu_unhandled,
        diagnostics.cdrom_unimplemented,
        diagnostics.bus_unhandled_reads,
        diagnostics.bus_unhandled_writes,
    );

    assert!(
        reached,
        "a BIOS não chegou ao shell em {BOOT_BUDGET} ciclos (pc=0x{:08X})",
        system.cpu().pc()
    );
}

#[test]
fn um_frame_com_a_bios_real_nao_entra_em_panico() {
    let (_, bytes) = bios_or_skip!();

    let bios = Bios::new(bytes).expect("BIOS de 512 KB deve ser aceita");
    let mut system = System::new(bios);

    // Um segundo de console. O que importa aqui não é o resultado visual, e
    // sim que nenhum caminho do core estoure com a BIOS de verdade.
    for _ in 0..60 {
        system.run_frame();
    }

    assert_eq!(
        system.framebuffer().len() % 4,
        0,
        "o framebuffer tem que continuar em RGBA8"
    );
}
