//! Imagens de disco: ISO, BIN cru e folhas CUE.
//!
//! Referência: PSX-SPX — "CDROM Disk Format", "CDROM Sector Encoding".
//!
//! O core não abre arquivos. O embedder entrega os bytes já lidos e, no caso
//! de um CUE, o texto da folha junto — quem sabe achar o `.bin` ao lado do
//! `.cue` é quem tem sistema de arquivos (ou a `File API`, no navegador).

use core::fmt;

/// Um setor cru do CD, com sync, header e ECC.
pub const SECTOR_RAW: usize = 2352;

/// Dados de usuário de um setor Mode 1 ou Mode 2 Form 1.
pub const SECTOR_USER: usize = 2048;

/// Offset dos dados de usuário num setor Mode 1 cru: sync(12) + header(4).
const MODE1_DATA_OFFSET: usize = 16;

/// Offset num setor Mode 2 Form 1: sync(12) + header(4) + subheader(8).
const MODE2_FORM1_DATA_OFFSET: usize = 24;

/// Offset do byte de modo dentro do header.
const MODE_BYTE_OFFSET: usize = 15;

/// Pregap obrigatório antes da faixa 1: 2 segundos = 150 setores.
///
/// É por isso que o LBA 0 corresponde a MSF 00:02:00, e não a 00:00:00.
pub const PREGAP_SECTORS: u32 = 150;

/// Padrão de sincronismo que abre todo setor cru.
const SYNC_PATTERN: [u8; 12] = [
    0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00,
];

/// Posição no disco em Minuto:Segundo:Frame.
///
/// Guardada em binário. A conversão para BCD acontece só na fronteira com o
/// hardware, que é onde ela existe de verdade.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Msf {
    pub minute: u8,
    pub second: u8,
    pub frame: u8,
}

impl Msf {
    pub const fn new(minute: u8, second: u8, frame: u8) -> Self {
        Self {
            minute,
            second,
            frame,
        }
    }

    /// Converte de BCD, como o hardware entrega em `Setloc`.
    pub const fn from_bcd(minute: u8, second: u8, frame: u8) -> Self {
        Self {
            minute: from_bcd(minute),
            second: from_bcd(second),
            frame: from_bcd(frame),
        }
    }

    /// Devolve os três campos em BCD, como `GetlocL` e `GetTD` esperam.
    pub const fn to_bcd(self) -> [u8; 3] {
        [to_bcd(self.minute), to_bcd(self.second), to_bcd(self.frame)]
    }

    /// Endereço absoluto em setores, contando o pregap.
    pub const fn to_absolute(self) -> u32 {
        (self.minute as u32 * 60 + self.second as u32) * 75 + self.frame as u32
    }

    /// LBA relativo ao início dos dados, já descontado o pregap.
    pub const fn to_lba(self) -> u32 {
        self.to_absolute().saturating_sub(PREGAP_SECTORS)
    }

    pub const fn from_lba(lba: u32) -> Self {
        let absolute = lba + PREGAP_SECTORS;
        Self {
            minute: (absolute / (60 * 75)) as u8,
            second: ((absolute / 75) % 60) as u8,
            frame: (absolute % 75) as u8,
        }
    }
}

impl fmt::Display for Msf {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:02}:{:02}:{:02}", self.minute, self.second, self.frame)
    }
}

const fn from_bcd(value: u8) -> u8 {
    (value >> 4) * 10 + (value & 0x0F)
}

const fn to_bcd(value: u8) -> u8 {
    ((value / 10) << 4) | (value % 10)
}

/// Tipo de faixa, que decide se o setor é dado ou áudio.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackKind {
    Data,
    Audio,
}

/// Uma faixa dentro da imagem.
#[derive(Debug, Clone)]
pub struct Track {
    pub number: u8,
    pub kind: TrackKind,
    /// Primeiro LBA da faixa.
    pub start_lba: u32,
    /// Quantos setores a faixa ocupa.
    pub length: u32,
    /// Bytes por setor **na imagem** — 2048 num ISO, 2352 num BIN cru.
    pub sector_size: usize,
    /// Onde a faixa começa dentro do arquivo, em bytes.
    pub file_offset: u64,
}

/// Região gravada na área de licença do disco.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscRegion {
    America,
    Europe,
    Japan,
}

impl DiscRegion {
    /// Os quatro bytes que `GetID` devolve.
    pub const fn id_bytes(self) -> [u8; 4] {
        match self {
            DiscRegion::America => *b"SCEA",
            DiscRegion::Europe => *b"SCEE",
            DiscRegion::Japan => *b"SCEI",
        }
    }
}

/// Erro ao interpretar uma imagem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiscError {
    /// A imagem não tem tamanho múltiplo de nenhum formato conhecido.
    UnknownFormat { length: usize },
    /// A imagem está vazia.
    Empty,
    /// O CUE referencia uma faixa antes de declarar o arquivo.
    CueTrackWithoutFile { line: usize },
    /// Uma linha do CUE tem sintaxe que não sabemos ler.
    CueSyntax { line: usize, text: String },
    /// O CUE não declarou nenhuma faixa.
    CueWithoutTracks,
}

impl fmt::Display for DiscError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DiscError::UnknownFormat { length } => write!(
                f,
                "imagem de {length} bytes não é múltiplo de {SECTOR_USER} (ISO) nem de {SECTOR_RAW} (BIN)"
            ),
            DiscError::Empty => write!(f, "imagem de disco vazia"),
            DiscError::CueTrackWithoutFile { line } => {
                write!(f, "linha {line}: TRACK antes de qualquer FILE")
            }
            DiscError::CueSyntax { line, text } => {
                write!(f, "linha {line}: não entendi \"{text}\"")
            }
            DiscError::CueWithoutTracks => write!(f, "o CUE não declara nenhuma faixa"),
        }
    }
}

impl std::error::Error for DiscError {}

/// Uma imagem de disco carregada na memória.
///
/// A imagem inteira fica em RAM. Para um jogo de 700 MB isso é muito, e a
/// evolução natural é o embedder entregar setores sob demanda — a interface
/// de leitura já é por LBA justamente para permitir essa troca sem mexer no
/// controlador.
#[derive(Clone)]
pub struct Disc {
    image: Vec<u8>,
    tracks: Vec<Track>,
    region: DiscRegion,
}

impl fmt::Debug for Disc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Sem o Vec: um Debug de 700 MB não ajuda ninguém.
        f.debug_struct("Disc")
            .field("bytes", &self.image.len())
            .field("tracks", &self.tracks.len())
            .field("region", &self.region)
            .finish()
    }
}

impl Disc {
    /// Interpreta uma imagem sem folha CUE: um ISO ou um BIN de faixa única.
    ///
    /// O formato é deduzido do padrão de sincronismo no início do arquivo, e
    /// não da extensão — arquivos chegam renomeados o tempo todo.
    pub fn from_image(image: Vec<u8>) -> Result<Self, DiscError> {
        if image.is_empty() {
            return Err(DiscError::Empty);
        }

        let sector_size = if image.starts_with(&SYNC_PATTERN) {
            SECTOR_RAW
        } else if image.len() % SECTOR_USER == 0 {
            SECTOR_USER
        } else if image.len() % SECTOR_RAW == 0 {
            // Sem sync no começo, mas o tamanho fecha com setor cru: aceita.
            SECTOR_RAW
        } else {
            return Err(DiscError::UnknownFormat {
                length: image.len(),
            });
        };

        let length = (image.len() / sector_size) as u32;
        let tracks = vec![Track {
            number: 1,
            kind: TrackKind::Data,
            start_lba: 0,
            length,
            sector_size,
            file_offset: 0,
        }];

        let mut disc = Self {
            image,
            tracks,
            region: DiscRegion::America,
        };
        disc.region = disc.detect_region();
        Ok(disc)
    }

    /// Interpreta uma folha CUE junto do binário que ela referencia.
    ///
    /// Só o caso de um único arquivo é suportado, que é o formato em que
    /// praticamente todo jogo de PSX circula. CUEs multi-arquivo devolvem as
    /// faixas apontando para o mesmo binário, o que estaria errado — por isso
    /// um `FILE` extra é recusado em vez de aceito em silêncio.
    pub fn from_cue(cue: &str, image: Vec<u8>) -> Result<Self, DiscError> {
        if image.is_empty() {
            return Err(DiscError::Empty);
        }

        let mut tracks: Vec<Track> = Vec::new();
        let mut files_seen = 0usize;
        let mut pending: Option<(u8, TrackKind, usize)> = None;

        for (index, raw) in cue.lines().enumerate() {
            let line = raw.trim();
            let number = index + 1;
            if line.is_empty() {
                continue;
            }
            let mut parts = line.split_whitespace();
            let Some(keyword) = parts.next() else {
                continue;
            };

            match keyword.to_ascii_uppercase().as_str() {
                "FILE" => {
                    files_seen += 1;
                    if files_seen > 1 {
                        return Err(DiscError::CueSyntax {
                            line: number,
                            text: "CUE com múltiplos FILE ainda não é suportado".into(),
                        });
                    }
                }
                "TRACK" => {
                    let track_number = parts
                        .next()
                        .and_then(|value| value.parse::<u8>().ok())
                        .ok_or_else(|| DiscError::CueSyntax {
                            line: number,
                            text: line.into(),
                        })?;
                    let mode = parts.next().unwrap_or("MODE1/2352").to_ascii_uppercase();
                    if files_seen == 0 {
                        return Err(DiscError::CueTrackWithoutFile { line: number });
                    }
                    let kind = if mode.starts_with("AUDIO") {
                        TrackKind::Audio
                    } else {
                        TrackKind::Data
                    };
                    // O tamanho do setor vem depois da barra: MODE1/2352.
                    let sector_size = mode
                        .rsplit('/')
                        .next()
                        .and_then(|value| value.parse::<usize>().ok())
                        .unwrap_or(SECTOR_RAW);
                    pending = Some((track_number, kind, sector_size));
                }
                "INDEX" => {
                    let Some((track_number, kind, sector_size)) = pending else {
                        continue;
                    };
                    let index_number = parts.next().and_then(|value| value.parse::<u8>().ok());
                    // INDEX 00 é o pregap; a faixa começa de fato no INDEX 01.
                    if index_number != Some(1) {
                        continue;
                    }
                    let position = parts.next().ok_or_else(|| DiscError::CueSyntax {
                        line: number,
                        text: line.into(),
                    })?;
                    let msf = parse_cue_msf(position).ok_or_else(|| DiscError::CueSyntax {
                        line: number,
                        text: line.into(),
                    })?;
                    // No CUE o MSF é posição dentro do arquivo, sem pregap.
                    let start = msf.to_absolute();
                    tracks.push(Track {
                        number: track_number,
                        kind,
                        start_lba: start,
                        // Corrigido no fecho, quando sabemos onde termina.
                        length: 0,
                        sector_size,
                        file_offset: start as u64 * sector_size as u64,
                    });
                    pending = None;
                }
                // TITLE, PERFORMER, REM, PREGAP, POSTGAP, FLAGS: irrelevantes
                // para localizar setores.
                _ => {}
            }
        }

        if tracks.is_empty() {
            return Err(DiscError::CueWithoutTracks);
        }

        // Cada faixa vai até o começo da seguinte; a última até o fim do
        // arquivo.
        for index in 0..tracks.len() {
            let start = tracks[index].start_lba;
            let end = match tracks.get(index + 1) {
                Some(next) => next.start_lba,
                None => (image.len() / tracks[index].sector_size) as u32,
            };
            tracks[index].length = end.saturating_sub(start);
        }

        let mut disc = Self {
            image,
            tracks,
            region: DiscRegion::America,
        };
        disc.region = disc.detect_region();
        Ok(disc)
    }

    pub fn tracks(&self) -> &[Track] {
        &self.tracks
    }

    pub const fn region(&self) -> DiscRegion {
        self.region
    }

    /// Número da primeira e da última faixa, como `GetTN` devolve.
    pub fn track_range(&self) -> (u8, u8) {
        let first = self.tracks.first().map_or(1, |track| track.number);
        let last = self.tracks.last().map_or(1, |track| track.number);
        (first, last)
    }

    /// Início de uma faixa. O número 0 significa "fim do disco", que é como o
    /// hardware reporta a duração total em `GetTD`.
    pub fn track_start(&self, number: u8) -> Option<Msf> {
        if number == 0 {
            return Some(Msf::from_lba(self.total_sectors()));
        }
        self.tracks
            .iter()
            .find(|track| track.number == number)
            .map(|track| Msf::from_lba(track.start_lba))
    }

    pub fn total_sectors(&self) -> u32 {
        self.tracks
            .last()
            .map_or(0, |track| track.start_lba + track.length)
    }

    fn track_for(&self, lba: u32) -> Option<&Track> {
        self.tracks
            .iter()
            .find(|track| lba >= track.start_lba && lba < track.start_lba + track.length)
    }

    /// Os 2048 bytes de dados de usuário de um setor.
    ///
    /// Funciona tanto para Mode 1 quanto para Mode 2 Form 1: o offset dos
    /// dados é decidido pelo byte de modo do próprio setor, não por
    /// configuração — discos de PSX misturam os dois.
    pub fn read_user_data(&self, lba: u32) -> Option<&[u8]> {
        let track = self.track_for(lba)?;
        let offset = self.byte_offset(track, lba);

        if track.sector_size == SECTOR_USER {
            return self.image.get(offset..offset + SECTOR_USER);
        }

        let sector = self.image.get(offset..offset + SECTOR_RAW)?;
        let data_offset = match sector[MODE_BYTE_OFFSET] {
            2 => MODE2_FORM1_DATA_OFFSET,
            _ => MODE1_DATA_OFFSET,
        };
        sector.get(data_offset..data_offset + SECTOR_USER)
    }

    /// Os 2340 bytes a partir do header, que é o que o modo "setor inteiro"
    /// do `Setmode` entrega — o sync de 12 bytes fica de fora.
    pub fn read_whole_sector(&self, lba: u32) -> Option<&[u8]> {
        let track = self.track_for(lba)?;
        if track.sector_size != SECTOR_RAW {
            // Num ISO não existem header nem ECC para entregar.
            return None;
        }
        let offset = self.byte_offset(track, lba) + SYNC_PATTERN.len();
        self.image
            .get(offset..offset + (SECTOR_RAW - SYNC_PATTERN.len()))
    }

    /// O subheader de um setor Mode 2: arquivo, canal, submodo e codificação.
    ///
    /// É o que distingue um setor de dados de um de áudio XA. Só existe em
    /// imagens cruas de Mode 2 — num ISO de 2048 bytes não há subheader, e
    /// todo setor é de dados.
    pub fn subheader(&self, lba: u32) -> Option<[u8; 4]> {
        let track = self.track_for(lba)?;
        if track.sector_size != SECTOR_RAW {
            return None;
        }
        let offset = self.byte_offset(track, lba);
        let sector = self.image.get(offset..offset + SECTOR_RAW)?;
        if sector[MODE_BYTE_OFFSET] != 2 {
            return None;
        }
        Some([sector[16], sector[17], sector[18], sector[19]])
    }

    fn byte_offset(&self, track: &Track, lba: u32) -> usize {
        (track.file_offset + (lba - track.start_lba) as u64 * track.sector_size as u64) as usize
    }

    /// Lê a região da área de licença (setores 4 a 15).
    ///
    /// O texto ali é o que o BIOS confere para decidir se o disco roda. Se
    /// nada casar, assumimos América — errar a região faz o jogo recusar o
    /// boot, e é melhor um palpite do que um `None` que trava tudo.
    fn detect_region(&self) -> DiscRegion {
        for lba in 4..16 {
            let Some(data) = self.read_user_data(lba) else {
                continue;
            };
            let text = String::from_utf8_lossy(data);
            if text.contains("Sony Computer Entertainment Inc.") {
                return DiscRegion::Japan;
            }
            if text.contains("of America") {
                return DiscRegion::America;
            }
            if text.contains("Europe") {
                return DiscRegion::Europe;
            }
        }
        DiscRegion::America
    }
}

/// Interpreta o `MM:SS:FF` de uma linha `INDEX` do CUE.
fn parse_cue_msf(text: &str) -> Option<Msf> {
    let mut parts = text.split(':');
    let minute = parts.next()?.parse::<u8>().ok()?;
    let second = parts.next()?.parse::<u8>().ok()?;
    let frame = parts.next()?.parse::<u8>().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some(Msf::new(minute, second, frame))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Monta um setor cru Mode 1 com `fill` nos dados de usuário.
    fn raw_sector(fill: u8) -> Vec<u8> {
        let mut sector = vec![0u8; SECTOR_RAW];
        sector[..12].copy_from_slice(&SYNC_PATTERN);
        sector[MODE_BYTE_OFFSET] = 1;
        sector[MODE1_DATA_OFFSET..MODE1_DATA_OFFSET + SECTOR_USER].fill(fill);
        sector
    }

    #[test]
    fn msf_zero_is_the_end_of_the_pregap() {
        assert_eq!(Msf::new(0, 2, 0).to_lba(), 0);
        assert_eq!(Msf::from_lba(0), Msf::new(0, 2, 0));
    }

    #[test]
    fn msf_round_trips_through_lba() {
        for lba in [0, 1, 74, 75, 4499, 4500, 100_000] {
            assert_eq!(Msf::from_lba(lba).to_lba(), lba, "lba {lba}");
        }
    }

    #[test]
    fn msf_converts_to_and_from_bcd() {
        let msf = Msf::from_bcd(0x12, 0x34, 0x56);
        assert_eq!(msf, Msf::new(12, 34, 56));
        assert_eq!(msf.to_bcd(), [0x12, 0x34, 0x56]);
    }

    #[test]
    fn an_iso_is_recognised_by_its_sector_size() {
        let image = vec![0xAAu8; SECTOR_USER * 4];
        let disc = Disc::from_image(image).expect("ISO válido");
        assert_eq!(disc.tracks().len(), 1);
        assert_eq!(disc.tracks()[0].sector_size, SECTOR_USER);
        assert_eq!(disc.total_sectors(), 4);
        assert_eq!(disc.read_user_data(2).unwrap()[0], 0xAA);
    }

    #[test]
    fn a_raw_bin_is_recognised_by_the_sync_pattern() {
        let mut image = Vec::new();
        image.extend(raw_sector(0x11));
        image.extend(raw_sector(0x22));
        let disc = Disc::from_image(image).expect("BIN válido");
        assert_eq!(disc.tracks()[0].sector_size, SECTOR_RAW);
        assert_eq!(disc.read_user_data(0).unwrap()[0], 0x11);
        assert_eq!(disc.read_user_data(1).unwrap()[0], 0x22);
    }

    #[test]
    fn mode2_form1_data_starts_after_the_subheader() {
        let mut sector = raw_sector(0);
        sector[MODE_BYTE_OFFSET] = 2;
        sector[MODE2_FORM1_DATA_OFFSET..MODE2_FORM1_DATA_OFFSET + SECTOR_USER].fill(0x5A);
        let disc = Disc::from_image(sector).expect("setor Mode 2");
        assert_eq!(disc.read_user_data(0).unwrap()[0], 0x5A);
    }

    #[test]
    fn reading_past_the_end_gives_nothing() {
        let disc = Disc::from_image(vec![0u8; SECTOR_USER]).unwrap();
        assert!(disc.read_user_data(0).is_some());
        assert!(disc.read_user_data(1).is_none());
    }

    #[test]
    fn an_empty_image_is_rejected() {
        // `Disc` não é `PartialEq` de propósito (comparar 700 MB não faz
        // sentido), então o erro é conferido pelo padrão.
        assert!(matches!(
            Disc::from_image(Vec::new()),
            Err(DiscError::Empty)
        ));
    }

    #[test]
    fn an_image_with_no_valid_sector_size_is_rejected() {
        let error = Disc::from_image(vec![0u8; 1000]).unwrap_err();
        assert!(matches!(error, DiscError::UnknownFormat { length: 1000 }));
    }

    #[test]
    fn a_single_track_cue_is_parsed() {
        let cue = concat!(
            "FILE \"jogo.bin\" BINARY\n",
            "  TRACK 01 MODE2/2352\n",
            "    INDEX 01 00:00:00\n"
        );
        let image = vec![0u8; SECTOR_RAW * 10];
        let disc = Disc::from_cue(cue, image).expect("CUE válido");
        assert_eq!(disc.tracks().len(), 1);
        assert_eq!(disc.tracks()[0].kind, TrackKind::Data);
        assert_eq!(disc.tracks()[0].start_lba, 0);
        assert_eq!(disc.tracks()[0].length, 10);
    }

    #[test]
    fn a_cue_with_audio_tracks_splits_them_at_the_index_boundaries() {
        let cue = concat!(
            "FILE \"jogo.bin\" BINARY\n",
            "  TRACK 01 MODE2/2352\n",
            "    INDEX 01 00:00:00\n",
            "  TRACK 02 AUDIO\n",
            "    INDEX 00 00:00:04\n",
            "    INDEX 01 00:00:06\n"
        );
        let image = vec![0u8; SECTOR_RAW * 10];
        let disc = Disc::from_cue(cue, image).expect("CUE válido");

        assert_eq!(disc.tracks().len(), 2);
        // A faixa 1 termina onde a 2 começa — no INDEX 01, não no INDEX 00.
        assert_eq!(disc.tracks()[0].length, 6);
        assert_eq!(disc.tracks()[1].kind, TrackKind::Audio);
        assert_eq!(disc.tracks()[1].start_lba, 6);
        assert_eq!(disc.tracks()[1].length, 4);
        assert_eq!(disc.track_range(), (1, 2));
    }

    #[test]
    fn a_cue_without_tracks_is_rejected() {
        let error = Disc::from_cue("FILE \"x.bin\" BINARY\n", vec![0u8; 2352]).unwrap_err();
        assert_eq!(error, DiscError::CueWithoutTracks);
    }

    #[test]
    fn a_cue_with_multiple_files_is_rejected_instead_of_read_wrong() {
        let cue = concat!(
            "FILE \"a.bin\" BINARY\n",
            "  TRACK 01 MODE2/2352\n",
            "    INDEX 01 00:00:00\n",
            "FILE \"b.bin\" BINARY\n",
            "  TRACK 02 AUDIO\n",
            "    INDEX 01 00:00:00\n"
        );
        let error = Disc::from_cue(cue, vec![0u8; SECTOR_RAW * 4]).unwrap_err();
        assert!(matches!(error, DiscError::CueSyntax { line: 4, .. }));
    }

    #[test]
    fn track_zero_reports_the_end_of_the_disc() {
        let disc = Disc::from_image(vec![0u8; SECTOR_USER * 100]).unwrap();
        assert_eq!(disc.track_start(0), Some(Msf::from_lba(100)));
        assert_eq!(disc.track_start(1), Some(Msf::from_lba(0)));
        assert_eq!(disc.track_start(9), None);
    }

    #[test]
    fn the_region_comes_from_the_licence_area() {
        let mut image = vec![0u8; SECTOR_USER * 20];
        let text = b"          Licensed  by          Sony Computer Entertainment of America ";
        image[SECTOR_USER * 4..SECTOR_USER * 4 + text.len()].copy_from_slice(text);
        let disc = Disc::from_image(image).unwrap();
        assert_eq!(disc.region(), DiscRegion::America);
        assert_eq!(disc.region().id_bytes(), *b"SCEA");
    }
}
