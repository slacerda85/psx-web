//! Roda os testes de hardware do `ps1-tests` e compara com o console real.
//!
//! <https://github.com/JaCzekanski/ps1-tests> (MIT) traz, ao lado de cada
//! `.exe`, o `psx.log` que o **PlayStation de verdade** produziu. Comparar a
//! nossa saída com esse arquivo é uma referência melhor do que comparar com
//! outro emulador: o log veio do silício.
//!
//! ```sh
//! cargo run --release -p psx-core --example hwtest -- \
//!     --bios bios/SCPH1001.BIN --tests caminho/para/ps1-tests
//! ```
//!
//! Um teste específico:
//!
//! ```sh
//! cargo run --release -p psx-core --example hwtest -- --only gte/test-all
//! ```

use std::path::{Path, PathBuf};

#[path = "common/disc.rs"]
mod disc_loader;

use psx_core::{Bios, System};

/// Ciclos gastos deixando o kernel pronto antes de injetar o executável.
const BOOT_BUDGET: u64 = 500_000_000;

/// Frames executados depois da carga. Os testes imprimem e param.
const RUN_FRAMES: u32 = 600;

/// Frames sem saída nova antes de considerar o teste terminado.
const IDLE_FRAMES: u32 = 90;

struct Options {
    bios: String,
    tests: String,
    /// Imagem de disco a inserir. Os testes de CD-ROM exigem uma.
    disc: Option<String>,
    only: Option<String>,
    verbose: bool,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let options = parse_args()?;
    let bios = std::fs::read(&options.bios)?;

    let mut cases = collect(Path::new(&options.tests))?;
    cases.sort();
    if let Some(filter) = &options.only {
        cases.retain(|case| case.to_string_lossy().replace('\\', "/").contains(filter));
    }
    if cases.is_empty() {
        return Err(format!("nenhum teste com psx.log em {}", options.tests).into());
    }

    println!("{} testes com log de referência\n", cases.len());
    let mut passed = 0usize;
    let mut failed: Vec<String> = Vec::new();

    for case in &cases {
        let name = case
            .strip_prefix(&options.tests)
            .unwrap_or(case)
            .to_string_lossy()
            .replace('\\', "/");
        let name = name.trim_start_matches('/').to_string();

        let expected = std::fs::read_to_string(case.join("psx.log"))?;
        let Some(executable) = find_exe(case)? else {
            println!("  ??  {name}  (sem .exe ao lado do log)");
            continue;
        };

        let actual = run(&bios, &executable, options.disc.as_deref())?;

        if contains_in_order(&normalise(&actual), &normalise(&expected)) {
            println!("  ok  {name}");
            passed += 1;
        } else {
            println!("  XX  {name}");
            failed.push(name.clone());
            if options.verbose {
                report_difference(&expected, &actual);
            }
        }
    }

    println!("\n{passed} iguais ao hardware, {} diferentes", failed.len());
    for name in &failed {
        println!("  diferente: {name}");
    }
    if !failed.is_empty() && !options.verbose {
        println!("\nUse --verbose para ver as diferenças linha a linha.");
    }
    Ok(())
}

/// Executa um `.exe` de teste e devolve o que ele imprimiu.
fn run(
    bios: &[u8],
    executable: &Path,
    disc: Option<&str>,
) -> Result<String, Box<dyn std::error::Error>> {
    let mut system = System::new(Bios::new(bios.to_vec())?);
    // Os testes de CD-ROM param logo no começo pedindo um disco.
    if let Some(path) = disc {
        disc_loader::load(&mut system, Path::new(path))?;
    }
    // O kernel precisa estar de pé antes do sideload: os testes chamam funções
    // do BIOS para imprimir.
    system.run_until_shell(BOOT_BUDGET);
    system.take_tty();
    system.load_exe(&std::fs::read(executable)?)?;

    let mut output = String::new();
    let mut idle = 0u32;
    for _ in 0..RUN_FRAMES {
        system.run_frame();
        let chunk = system.take_tty();
        if chunk.is_empty() {
            idle += 1;
            // Sem saída nova por tempo suficiente: o teste terminou (ou travou).
            if idle >= IDLE_FRAMES && !output.is_empty() {
                break;
            }
        } else {
            idle = 0;
            output.push_str(&chunk);
        }
    }
    Ok(output)
}

/// `true` se toda linha de `expected` aparece em `actual`, na mesma ordem.
///
/// Comparação exata não serve: a nossa saída carrega junto o que o próprio
/// BIOS imprime no boot (`ResetGraph:...`), que o log de referência não tem, e
/// algumas versões dos testes imprimem uma linha de resumo a mais. O que
/// importa é que tudo o que o console real disse, nós também dissemos, na
/// mesma ordem — uma linha faltando ou fora de ordem reprova.
fn contains_in_order(actual: &[String], expected: &[String]) -> bool {
    let mut lines = actual.iter();
    expected
        .iter()
        .all(|wanted| lines.any(|line| line == wanted))
}

/// Normaliza para comparação: fim de linha e espaços em branco nas pontas.
fn normalise(text: &str) -> Vec<String> {
    text.replace('\r', "")
        .lines()
        // Alguns logs de referência foram capturados com um prefixo "% " por
        // linha; ele não faz parte do que o teste imprime.
        .map(|line| line.trim_end().trim_start_matches("% ").to_string())
        .skip_while(|line| line.is_empty())
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .skip_while(|line| line.is_empty())
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

fn report_difference(expected: &str, actual: &str) {
    let expected = normalise(expected);
    let actual = normalise(actual);
    let width = expected.len().max(actual.len());
    for index in 0..width {
        let left = expected.get(index).map(String::as_str).unwrap_or("<nada>");
        let right = actual.get(index).map(String::as_str).unwrap_or("<nada>");
        if left == right {
            println!("        {left}");
        } else {
            println!("      - hardware: {left}");
            println!("      + nosso:    {right}");
        }
    }
}

/// Diretórios que têm um `psx.log` de referência.
fn collect(root: &Path) -> Result<Vec<PathBuf>, Box<dyn std::error::Error>> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(directory) = stack.pop() {
        for entry in std::fs::read_dir(&directory)?.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.file_name().is_some_and(|name| name == "psx.log") {
                found.push(directory.clone());
            }
        }
    }
    Ok(found)
}

fn find_exe(directory: &Path) -> Result<Option<PathBuf>, Box<dyn std::error::Error>> {
    for entry in std::fs::read_dir(directory)?.flatten() {
        let path = entry.path();
        if path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"))
        {
            return Ok(Some(path));
        }
    }
    Ok(None)
}

fn parse_args() -> Result<Options, Box<dyn std::error::Error>> {
    let mut options = Options {
        bios: "bios/SCPH1001.BIN".into(),
        tests: "ps1-tests".into(),
        disc: None,
        only: None,
        verbose: false,
    };
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let mut value = || {
            args.next()
                .ok_or_else(|| format!("{flag} precisa de um valor"))
        };
        match flag.as_str() {
            "--bios" => options.bios = value()?,
            "--tests" => options.tests = value()?,
            "--disc" => options.disc = Some(value()?),
            "--only" => options.only = Some(value()?),
            "--verbose" => options.verbose = true,
            other => return Err(format!("opção desconhecida: {other}").into()),
        }
    }
    Ok(options)
}
