//! Bindings `wasm-bindgen` do core de emulação.
//!
//! Esta crate é deliberadamente fina: ela não toma nenhuma decisão de
//! emulação, só traduz tipos entre JavaScript e [`psx_core`]. Toda a lógica
//! vive em `psx-core`, que continua testável fora do navegador.
//!
//! Contrato com o frontend:
//!
//! 1. `new Emulator(biosBytes)` — a BIOS é fornecida pelo usuário.
//! 2. `runFrame()` a cada `requestAnimationFrame`.
//! 3. `framebufferPtr()` + `framebufferLength()` descrevem uma região RGBA8
//!    dentro da memória linear do WASM. O frontend monta um `Uint8Array` sobre
//!    ela — zero cópia. A view é invalidada se a memória do WASM crescer, então
//!    o frontend deve remontá-la a cada frame.
//! 4. `drainAudio(buffer)` alimenta o AudioWorklet.

#![forbid(unsafe_code)]

use psx_core::sio::ButtonState;
use psx_core::{Bios, Region, System};
use wasm_bindgen::prelude::*;

/// Um console PSX exposto ao JavaScript.
#[wasm_bindgen]
pub struct Emulator {
    system: System,
    audio_scratch: Vec<i16>,
}

#[wasm_bindgen]
impl Emulator {
    /// Cria o console com a BIOS fornecida pelo usuário (512 KB).
    ///
    /// Nenhuma BIOS acompanha este projeto: o arquivo tem que vir do console
    /// do próprio usuário.
    #[wasm_bindgen(constructor)]
    pub fn new(bios: Vec<u8>) -> Result<Emulator, JsError> {
        let bios = Bios::new(bios).map_err(|error| JsError::new(&error.to_string()))?;
        Ok(Self {
            system: System::new(bios),
            audio_scratch: vec![0; 8192],
        })
    }

    /// Reinicia o console.
    pub fn reset(&mut self) {
        self.system.reset();
    }

    /// Alterna entre NTSC (`false`) e PAL (`true`).
    #[wasm_bindgen(js_name = setPalRegion)]
    pub fn set_pal_region(&mut self, pal: bool) {
        self.system
            .set_region(if pal { Region::Pal } else { Region::Ntsc });
    }

    /// Executa um frame de vídeo inteiro.
    #[wasm_bindgen(js_name = runFrame)]
    pub fn run_frame(&mut self) {
        self.system.run_frame();
    }

    /// Endereço do framebuffer RGBA8 dentro da memória linear do WASM.
    ///
    /// O frontend monta `new Uint8Array(memory.buffer, ptr, len)` sobre ele.
    /// Devolver o ponteiro em vez de uma view mantém este crate 100% seguro:
    /// quem constrói a view é o JavaScript.
    #[wasm_bindgen(js_name = framebufferPtr)]
    pub fn framebuffer_ptr(&self) -> *const u8 {
        self.system.framebuffer().as_ptr()
    }

    /// Comprimento útil do framebuffer: `frameWidth * frameHeight * 4`.
    #[wasm_bindgen(js_name = framebufferLength)]
    pub fn framebuffer_length(&self) -> usize {
        let length = (self.system.frame_width() * self.system.frame_height() * 4) as usize;
        length.min(self.system.framebuffer().len())
    }

    #[wasm_bindgen(js_name = frameWidth)]
    pub fn frame_width(&self) -> u32 {
        self.system.frame_width()
    }

    #[wasm_bindgen(js_name = frameHeight)]
    pub fn frame_height(&self) -> u32 {
        self.system.frame_height()
    }

    /// Atualiza os botões de um slot a partir de uma máscara ativo-**alta**.
    ///
    /// A ordem dos bits é a do protocolo SIO0: Select, L3, R3, Start, Up,
    /// Right, Down, Left, L2, R2, L1, R1, Triangle, Circle, Cross, Square.
    #[wasm_bindgen(js_name = setButtons)]
    pub fn set_buttons(&mut self, slot: usize, pressed_mask: u16) {
        self.system
            .set_buttons(slot, ButtonState::from_pressed_mask(pressed_mask));
    }

    /// Copia amostras de áudio (i16 estéreo intercalado) para `out`.
    /// Devolve quantos valores foram escritos.
    #[wasm_bindgen(js_name = drainAudio)]
    pub fn drain_audio(&mut self, out: &mut [f32]) -> usize {
        let wanted = out.len().min(self.audio_scratch.len());
        let written = self.system.drain_audio(&mut self.audio_scratch[..wanted]);
        for (destination, sample) in out.iter_mut().zip(&self.audio_scratch[..written]) {
            *destination = *sample as f32 / 32768.0;
        }
        written
    }

    /// Insere uma imagem de disco crua (ISO ou BIN de faixa única).
    ///
    /// O formato é deduzido do conteúdo, não da extensão.
    #[wasm_bindgen(js_name = loadDisc)]
    pub fn load_disc(&mut self, image: Vec<u8>) -> Result<(), JsError> {
        self.system
            .load_disc(image)
            .map_err(|error| JsError::new(&error.to_string()))
    }

    /// Insere uma imagem descrita por uma folha CUE.
    ///
    /// O JavaScript entrega os dois arquivos: o core não sabe achar o `.bin`
    /// que a folha referencia, e o nome dentro dela quase nunca sobrevive ao
    /// download.
    #[wasm_bindgen(js_name = loadDiscWithCue)]
    pub fn load_disc_with_cue(&mut self, cue: &str, image: Vec<u8>) -> Result<(), JsError> {
        self.system
            .load_disc_with_cue(cue, image)
            .map_err(|error| JsError::new(&error.to_string()))
    }

    /// Abre a bandeja.
    #[wasm_bindgen(js_name = ejectDisc)]
    pub fn eject_disc(&mut self) {
        self.system.eject_disc();
    }

    /// Descrição curta do disco inserido, para a UI mostrar. Vazio sem disco.
    #[wasm_bindgen(js_name = discInfo)]
    pub fn disc_info(&self) -> String {
        match self.system.disc() {
            Some(disc) => {
                let region = disc.region().id_bytes();
                format!(
                    "{} · {} setores · {} faixa(s)",
                    String::from_utf8_lossy(&region),
                    disc.total_sectors(),
                    disc.tracks().len()
                )
            }
            None => String::new(),
        }
    }

    /// Carrega um `PS-X EXE` (homebrew ou binário de teste) e salta para ele.
    #[wasm_bindgen(js_name = loadExe)]
    pub fn load_exe(&mut self, data: &[u8]) -> Result<(), JsError> {
        self.system
            .load_exe(data)
            .map_err(|error| JsError::new(&error.to_string()))
    }

    /// Executa até o BIOS entregar o controle ao shell.
    #[wasm_bindgen(js_name = runUntilShell)]
    pub fn run_until_shell(&mut self, max_cycles: f64) -> bool {
        self.system.run_until_shell(max_cycles as u64)
    }

    /// Contadores de funcionalidade ainda não implementada, em JSON.
    ///
    /// A UI mostra isso no painel de diagnóstico para deixar claro o que
    /// falta, em vez de o emulador falhar em silêncio.
    pub fn diagnostics(&self) -> String {
        let d = self.system.diagnostics();
        format!(
            "{{\"gteUnimplemented\":{},\"gpuUnhandled\":{},\"cdromUnimplemented\":{},\"busUnhandledReads\":{},\"busUnhandledWrites\":{}}}",
            d.gte_unimplemented,
            d.gpu_unhandled,
            d.cdrom_unimplemented,
            d.bus_unhandled_reads,
            d.bus_unhandled_writes
        )
    }
}

/// Versão do core, para a UI mostrar.
#[wasm_bindgen]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}
