//! Inspeciona uma imagem de disco e imprime o que o core entendeu dela.
//!
//! Serve para conferir o parsing contra imagens reais antes de culpar o
//! controlador por um jogo que não carrega.
//!
//! ```sh
//! cargo run -p psx-core --example disc_info -- games/xenogears/xenogears-disk-1.cue
//! ```
//!
//! Passando um `.cue`, o binário é procurado ao lado: primeiro pelo nome que a
//! folha declara e, se ele não existir, pelo único arquivo de dados da pasta —
//! imagens circulam renomeadas, e o nome dentro do CUE quase nunca acompanha.

use std::path::{Path, PathBuf};

use psx_core::cdrom::{Disc, SECTOR_RAW, SECTOR_USER};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .ok_or("uso: disc_info <arquivo.cue|.bin|.iso>")?;
    let path = PathBuf::from(path);

    let disc = if path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("cue"))
    {
        let cue = std::fs::read_to_string(&path)?;
        let binary = locate_binary(&path, &cue)?;
        println!("cue     : {}", path.display());
        println!("binário : {}", binary.display());
        Disc::from_cue(&cue, std::fs::read(&binary)?)?
    } else {
        println!("imagem  : {}", path.display());
        Disc::from_image(std::fs::read(&path)?)?
    };

    let total = disc.total_sectors();
    println!(
        "região  : {:?} ({})",
        disc.region(),
        unsafe_ascii(&disc.region().id_bytes())
    );
    println!(
        "duração : {} setores ({})",
        total,
        psx_core::cdrom::Msf::from_lba(total)
    );
    println!("faixas  : {}", disc.tracks().len());
    for track in disc.tracks() {
        println!(
            "  {:02} {:?}  início {}  {} setores  {} B/setor",
            track.number,
            track.kind,
            psx_core::cdrom::Msf::from_lba(track.start_lba),
            track.length,
            track.sector_size,
        );
    }

    // O volume descriptor ISO-9660 fica no setor 16 e começa com o byte de
    // tipo seguido de "CD001". Se isso bater, o sistema de arquivos do jogo
    // está onde deveria e a leitura de setores está alinhada.
    match disc.read_user_data(16) {
        Some(sector) if &sector[1..6] == b"CD001" => {
            let label = String::from_utf8_lossy(&sector[40..72]);
            println!("iso9660 : sim, volume \"{}\"", label.trim());
        }
        Some(_) => println!("iso9660 : setor 16 não tem o descritor (imagem desalinhada?)"),
        None => println!("iso9660 : imagem curta demais para ter o setor 16"),
    }

    println!("setor   : {SECTOR_USER} B de usuário, {SECTOR_RAW} B crus");
    Ok(())
}

/// Acha o binário de um CUE, tolerando que ele tenha sido renomeado.
fn locate_binary(cue_path: &Path, cue: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let directory = cue_path.parent().unwrap_or(Path::new("."));

    // Primeiro o nome declarado entre aspas na linha FILE.
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

    // Depois, o único arquivo da pasta que não é a própria folha.
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
    candidates.sort();
    // Um .bin de jogo é sempre o maior arquivo da pasta.
    candidates.sort_by_key(|path| std::cmp::Reverse(path.metadata().map(|m| m.len()).unwrap_or(0)));

    candidates
        .into_iter()
        .next()
        .ok_or_else(|| format!("nenhum binário ao lado de {}", cue_path.display()).into())
}

fn unsafe_ascii(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}
