/**
 * Ponto de entrada do frontend.
 *
 * Este arquivo e o unico que conhece todas as pecas ao mesmo tempo: ele liga
 * `Emulator` (WASM), `Renderer`, `Audio`, `Input`, `Ui` e `storage`. Cada
 * modulo abaixo dele continua ignorando os outros.
 */

import './styles.css';

import init, { Emulator, version } from './wasm/psx.js';
import { Renderer, type ScalingMode } from './renderer';
import { Audio } from './audio';
import { Input, DEFAULT_KEYMAP, type KeyMap } from './input';
import { storage } from './storage';
import { Ui, type DroppedKind } from './ui';

/** Duracao nominal de um frame, por regiao (PSX-SPX, "GPU Timings"). */
const FRAME_MS = { ntsc: 1000 / 59.94, pal: 1000 / 50 } as const;

/** Teto de frames por `requestAnimationFrame`. */
const MAX_CATCHUP_FRAMES = 4;

/** Quantos frames o avanco rapido executa por frame de video. */
const FAST_FORWARD_MULTIPLIER = 4;

/** Tamanho de uma BIOS de PSX. Serve para dar um erro util no arquivo errado. */
const BIOS_SIZE = 512 * 1024;

type Region = 'ntsc' | 'pal';

class App {
  private readonly ui = new Ui();
  private readonly audio = new Audio();
  private readonly input = new Input({ ...DEFAULT_KEYMAP });
  private renderer!: Renderer;
  private memory!: WebAssembly.Memory;

  private emulator: Emulator | null = null;
  private running = false;
  private fastForward = false;
  private region: Region = 'ntsc';

  private rafHandle = 0;
  private accumulator = 0;
  private lastTimestamp = 0;
  private framesThisSecond = 0;
  private fpsWindowStart = 0;
  private lastDiagnosticsAt = 0;

  async start(): Promise<void> {
    const wasm = await init();
    // `init()` devolve os exports do modulo; guardamos `memory` porque as
    // views sobre o framebuffer precisam ser remontadas a cada frame.
    this.memory = wasm.memory;
    this.ui.setVersion(version());

    try {
      this.renderer = Renderer.create(this.ui.canvas, 'sharp');
    } catch (error) {
      this.ui.notify(describe(error), 'error');
      throw error;
    }
    this.renderer.clear();

    this.input.attach();
    this.wireUi();
    await this.restoreSettings();
    this.watchGamepads();
  }

  // ---------------------------------------------------------------- UI

  private wireUi(): void {
    const { buttons, selects } = this.ui;

    buttons.run.addEventListener('click', () => void this.toggleRun());
    buttons.reset.addEventListener('click', () => this.reset());
    buttons.fast.addEventListener('click', () => {
      this.fastForward = !this.fastForward;
      buttons.fast.setAttribute('aria-pressed', String(this.fastForward));
      // Sem descartar a fila, o audio ficaria minutos atras da imagem.
      this.audio.flush();
    });

    buttons.mute.addEventListener('click', () => {
      const muted = buttons.mute.getAttribute('aria-pressed') !== 'true';
      buttons.mute.setAttribute('aria-pressed', String(muted));
      this.audio.setMuted(muted);
    });

    buttons.pickBios.addEventListener('click', () => {
      void this.pick('.bin,.rom,.BIN,.ROM', 'bios');
    });
    buttons.pickExe.addEventListener('click', () => {
      void this.pick('.exe,.psexe', 'exe');
    });

    buttons.keys.addEventListener('click', () => {
      this.ui.renderKeymap(this.input, () => void this.persistKeymap());
      this.ui.openKeymap();
    });
    buttons.keysReset.addEventListener('click', () => {
      this.input.load({ ...DEFAULT_KEYMAP });
      this.ui.renderKeymap(this.input, () => void this.persistKeymap());
      void this.persistKeymap();
    });

    selects.region.addEventListener('change', () => {
      this.region = selects.region.value === 'pal' ? 'pal' : 'ntsc';
      this.emulator?.setPalRegion(this.region === 'pal');
      this.accumulator = 0;
    });

    selects.scaling.addEventListener('change', () => {
      this.renderer.setScaling(selects.scaling.value as ScalingMode);
    });

    selects.volume.addEventListener('input', () => {
      this.audio.setVolume(Number(selects.volume.value) / 100);
    });

    this.ui.onFileDropped((file, kind) => void this.handleFile(file, kind));

    // Pausar ao trocar de aba evita que o acumulador de tempo estoure e o
    // emulador tente recuperar centenas de frames de uma vez ao voltar.
    document.addEventListener('visibilitychange', () => {
      if (document.hidden && this.running) void this.toggleRun();
    });
  }

  private async pick(accept: string, kind: DroppedKind): Promise<void> {
    const file = await this.ui.pickFile(accept);
    if (file) await this.handleFile(file, kind);
  }

  private async restoreSettings(): Promise<void> {
    if (!(await storage.available())) {
      this.ui.notify('Armazenamento local indisponível: as configurações não serão salvas.');
      return;
    }

    const keymap = await storage.loadKeymap();
    if (keymap) this.input.load(keymap as KeyMap);

    const bios = await storage.loadBios();
    if (bios) {
      this.bootWithBios(bios.bytes, bios.name, { persist: false });
    }
  }

  private async persistKeymap(): Promise<void> {
    try {
      await storage.saveKeymap(this.input.toJSON());
    } catch {
      // Falhar em salvar o mapa nao pode derrubar a sessao em andamento.
      this.ui.notify('Não foi possível salvar o mapeamento de teclas.');
    }
  }

  // ------------------------------------------------------------ arquivos

  private async handleFile(file: File, kind: DroppedKind): Promise<void> {
    const bytes = new Uint8Array(await file.arrayBuffer());

    if (kind === 'disc') {
      // O controlador de CD-ROM ainda nao consome imagens; dizer isso e
      // melhor do que aceitar o arquivo e nao rodar nada.
      this.ui.setChip('disc', 'warn', 'Disco (parcial)');
      this.ui.notify('Imagens de disco ainda não são suportadas — em implementação pelo agente @cdrom.', 'error');
      return;
    }

    if (kind === 'exe') {
      if (!this.emulator) {
        this.ui.notify('Carregue uma BIOS antes de rodar um .exe.', 'error');
        return;
      }
      try {
        this.emulator.loadExe(bytes);
        this.ui.notify(`${file.name} carregado.`);
        if (!this.running) await this.toggleRun();
      } catch (error) {
        this.ui.notify(describe(error), 'error');
      }
      return;
    }

    if (bytes.length !== BIOS_SIZE) {
      this.ui.notify(
        `BIOS inválida: esperados ${BIOS_SIZE} bytes, recebidos ${bytes.length}.`,
        'error',
      );
      return;
    }
    this.bootWithBios(bytes, file.name, { persist: true });
  }

  private bootWithBios(bytes: Uint8Array, name: string, options: { persist: boolean }): void {
    try {
      this.emulator?.free();
      this.emulator = new Emulator(bytes);
      this.emulator.setPalRegion(this.region === 'pal');
    } catch (error) {
      this.ui.notify(describe(error), 'error');
      return;
    }

    this.ui.setChip('bios', 'on', name.length > 18 ? `BIOS ${name.slice(0, 15)}…` : `BIOS ${name}`);
    this.ui.setOnboardingVisible(false);
    this.ui.setControlsEnabled(true);
    this.accumulator = 0;

    if (options.persist) {
      void storage.saveBios(name, bytes).catch(() => {
        this.ui.notify('A BIOS foi carregada, mas não pôde ser salva para a próxima visita.');
      });
    }
  }

  // -------------------------------------------------------------- loop

  private async toggleRun(): Promise<void> {
    if (!this.emulator) return;

    if (this.running) {
      this.running = false;
      cancelAnimationFrame(this.rafHandle);
      this.audio.flush();
      await this.audio.suspend();
      this.ui.setRunning(false);
      this.ui.setFps(null);
      return;
    }

    // O AudioContext so pode nascer dentro de um gesto do usuario, e este
    // metodo so e chamado a partir de um clique.
    try {
      await this.audio.start();
      this.ui.setChip('audio', 'on', `Áudio ${(this.audio.sampleRate ?? 0) / 1000} kHz`);
    } catch {
      this.ui.setChip('audio', 'warn', 'Áudio indisponível');
    }

    this.running = true;
    this.ui.setRunning(true);
    this.lastTimestamp = performance.now();
    this.fpsWindowStart = this.lastTimestamp;
    this.framesThisSecond = 0;
    this.accumulator = 0;
    this.rafHandle = requestAnimationFrame(this.tick);
  }

  private reset(): void {
    this.emulator?.reset();
    this.audio.flush();
    this.renderer.clear();
    this.accumulator = 0;
    this.ui.notify('Console reiniciado.');
  }

  private readonly tick = (timestamp: number): void => {
    if (!this.running || !this.emulator) return;
    this.rafHandle = requestAnimationFrame(this.tick);

    const emulator = this.emulator;
    const frameMs = FRAME_MS[this.region];
    const elapsed = timestamp - this.lastTimestamp;
    this.lastTimestamp = timestamp;

    // Um clamp no delta impede a "espiral da morte" depois de um travamento
    // longo: preferimos pular tempo a acumular uma divida impagavel.
    this.accumulator = Math.min(this.accumulator + elapsed, frameMs * MAX_CATCHUP_FRAMES);

    const multiplier = this.fastForward ? FAST_FORWARD_MULTIPLIER : 1;
    let ran = 0;
    while (this.accumulator >= frameMs && ran < MAX_CATCHUP_FRAMES) {
      emulator.setButtons(0, this.input.mask());
      for (let repeat = 0; repeat < multiplier; repeat++) emulator.runFrame();
      this.accumulator -= frameMs;
      ran++;
      this.framesThisSecond += multiplier;
    }

    if (ran > 0) {
      this.present(emulator);
      // Em avanco rapido o audio sairia acelerado; deixamos mudo de fato.
      if (!this.fastForward) this.audio.pump((out) => emulator.drainAudio(out));
      else this.audio.flush();
    }

    this.updateFps(timestamp);
    this.updateDiagnostics(timestamp, emulator);
  };

  private present(emulator: Emulator): void {
    const width = emulator.frameWidth();
    const height = emulator.frameHeight();
    const length = emulator.framebufferLength();
    if (length === 0) return;

    // A view precisa ser remontada a cada frame: qualquer crescimento da
    // memoria linear do WASM desanexa o ArrayBuffer anterior.
    const pixels = new Uint8Array(this.memory.buffer, emulator.framebufferPtr(), length);
    this.renderer.draw(pixels, width, height);
  }

  private updateFps(timestamp: number): void {
    const elapsed = timestamp - this.fpsWindowStart;
    if (elapsed < 500) return;
    this.ui.setFps((this.framesThisSecond * 1000) / elapsed);
    this.framesThisSecond = 0;
    this.fpsWindowStart = timestamp;
  }

  private updateDiagnostics(timestamp: number, emulator: Emulator): void {
    // Serializar JSON a 60 Hz seria desperdicio; 2 Hz basta para a UI.
    if (timestamp - this.lastDiagnosticsAt < 500) return;
    this.lastDiagnosticsAt = timestamp;
    try {
      this.ui.renderDiagnostics(JSON.parse(emulator.diagnostics()) as Record<string, number>);
    } catch {
      // Diagnostico e informativo: nunca deve interromper a emulacao.
    }
  }

  /** Reflete no chip se ha algum gamepad conectado. */
  private watchGamepads(): void {
    const refresh = () => {
      const pads = navigator.getGamepads?.() ?? [];
      const connected = pads.some((pad) => pad?.connected);
      this.ui.setChip('pad', connected ? 'on' : 'off', connected ? 'Gamepad' : 'Teclado');
    };
    window.addEventListener('gamepadconnected', refresh);
    window.addEventListener('gamepaddisconnected', refresh);
    refresh();
  }
}

function describe(error: unknown): string {
  if (error instanceof Error) return error.message;
  return String(error);
}

const app = new App();
app.start().catch((error: unknown) => {
  // Sem o WASM nao ha nada a fazer alem de contar ao usuario o motivo.
  document.body.textContent = `Falha ao iniciar o emulador: ${describe(error)}`;
});
