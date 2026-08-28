//! Carregamento de imagens de disco para os exemplos.
//!
//! O core não abre arquivos: ele diz quais o CUE referencia e recebe os bytes.
//! Quem tem sistema de arquivos é este módulo.

use std::path::{Path, PathBuf};

use psx_core::cdrom::Disc;
use psx_core::System;

/// Insere um disco, aceitando `.cue`, `.bin` ou `.iso`.
pub fn load(system: &mut System, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    if !path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("cue"))
    {
        system.load_disc(std::fs::read(path)?)?;
        return Ok(());
    }

    let cue = std::fs::read_to_string(path)?;
    let directory = path.parent().unwrap_or(Path::new("."));
    let mut images = Vec::new();

    for (index, declared) in Disc::cue_files(&cue).iter().enumerate() {
        match locate(directory, declared, index) {
            Some(file) => images.push(std::fs::read(file)?),
            None => {
                // Faixa declarada e não encontrada: entra vazia. A TOC continua
                // certa, que é o que o jogo consulta, e só a leitura dela falha.
                eprintln!("aviso: {declared} não foi encontrado ao lado do CUE");
                images.push(Vec::new());
            }
        }
    }

    system.load_disc_with_cue_files(&cue, images)?;
    Ok(())
}

/// Acha o arquivo que o CUE declara.
///
/// Imagens circulam renomeadas e o nome dentro da folha quase nunca acompanha,
/// então há três tentativas: o nome declarado, o mesmo nome do `.cue` com um
/// sufixo de faixa, e — só para o primeiro arquivo — o maior binário da pasta.
fn locate(directory: &Path, declared: &str, index: usize) -> Option<PathBuf> {
    let by_name = directory.join(declared);
    if by_name.is_file() {
        return Some(by_name);
    }

    let candidates: Vec<PathBuf> = std::fs::read_dir(directory)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && !path
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("cue"))
                && !path
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("html"))
        })
        .collect();

    // "jogo-track-2.bin" para o segundo FILE, e assim por diante.
    if index > 0 {
        let suffix = format!("track-{}", index + 1);
        if let Some(path) = candidates
            .iter()
            .find(|path| file_stem(path).contains(&suffix))
        {
            return Some(path.clone());
        }
        return None;
    }

    // O primeiro arquivo é o de dados: o maior da pasta, e nunca um que
    // pareça faixa extra.
    candidates
        .into_iter()
        .filter(|path| !file_stem(path).contains("track-"))
        .max_by_key(|path| path.metadata().map(|meta| meta.len()).unwrap_or(0))
}

fn file_stem(path: &Path) -> String {
    path.file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_ascii_lowercase()
}
