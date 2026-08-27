//! # psx-core
//!
//! Core de emulação do Sony PlayStation (SCPH-100x/550x/700x).
//!
//! Este crate é **lógica pura**: não faz I/O, não conhece o navegador e não
//! aloca nada por frame no caminho quente. O embedder (`psx-wasm`, testes ou
//! um frontend nativo) entrega os bytes da BIOS e da imagem de disco, chama
//! [`System::run_frame`] e lê o framebuffer resultante.
//!
//! Toda a implementação segue as especificações nocash
//! (<https://psx-spx.consoledev.net/>). Cada módulo de hardware cita, no topo,
//! a seção da spec que implementa.

#![forbid(unsafe_code)]

pub mod bios;
pub mod bus;
pub mod cdrom;
pub mod cpu;
pub mod dma;
pub mod exe;
pub mod gpu;
pub mod gte;
pub mod irq;
pub mod memory;
pub mod sio;
pub mod spu;
pub mod system;
pub mod timers;

pub use bios::Bios;
pub use system::{System, VIDEO_HEIGHT_MAX, VIDEO_WIDTH_MAX};

/// Clock da CPU R3000A em Hz.
///
/// PSX-SPX — "CPU Specifications": 33.8688 MHz, exatamente 44100 × 768.
pub const CPU_CLOCK_HZ: u32 = 33_868_800;

/// Região do console, que define a taxa de vídeo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Region {
    /// NTSC-U / NTSC-J — 60 Hz.
    #[default]
    Ntsc,
    /// PAL — 50 Hz.
    Pal,
}

impl Region {
    /// Ciclos de CPU por frame de vídeo.
    pub const fn cycles_per_frame(self) -> u32 {
        match self {
            // 33868800 / 60 e 33868800 / 50.
            Region::Ntsc => CPU_CLOCK_HZ / 60,
            Region::Pal => CPU_CLOCK_HZ / 50,
        }
    }

    /// Número de scanlines por frame (PSX-SPX — "GPU Timings").
    pub const fn scanlines_per_frame(self) -> u32 {
        match self {
            Region::Ntsc => 263,
            Region::Pal => 314,
        }
    }
}

/// Erros que o embedder pode receber ao alimentar o core.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PsxError {
    /// A imagem de BIOS não tem exatamente 512 KB.
    InvalidBiosSize(usize),
    /// O arquivo não começa com a assinatura `PS-X EXE`.
    InvalidExeMagic,
    /// O cabeçalho do executável é menor que os 2048 bytes obrigatórios.
    TruncatedExe(usize),
    /// O executável declara um destino fora da RAM de 2 MB.
    ExeOutOfRange { dest: u32, len: u32 },
}

impl core::fmt::Display for PsxError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            PsxError::InvalidBiosSize(got) => {
                write!(f, "BIOS deve ter 524288 bytes (512 KB), recebido {got}")
            }
            PsxError::InvalidExeMagic => write!(f, "arquivo não é um PS-X EXE"),
            PsxError::TruncatedExe(got) => {
                write!(f, "cabeçalho PS-X EXE truncado: {got} bytes")
            }
            PsxError::ExeOutOfRange { dest, len } => {
                write!(f, "PS-X EXE aponta para {dest:#010X} (+{len}) fora da RAM")
            }
        }
    }
}

impl std::error::Error for PsxError {}
