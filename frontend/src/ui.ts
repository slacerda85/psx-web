/**
 * Camada de DOM: indicadores, toasts, drag-and-drop e o dialogo de teclas.
 *
 * Nada aqui conhece o emulador — `main.ts` liga os dois. Isso mantem o loop
 * de emulacao livre de consulta ao DOM.
 */

import { BUTTONS, type ButtonName, type Input } from './input';

export type ChipName = 'bios' | 'disc' | 'pad' | 'audio';
export type ChipState = 'off' | 'on' | 'warn';

/** Extensoes aceitas por tipo de arquivo, para classificar o que foi solto. */
const EXE_EXTENSIONS = ['.exe', '.psexe', '.psx'];
const DISC_EXTENSIONS = ['.iso', '.cue', '.img', '.chd'];

/** Uma BIOS de PSX tem exatamente 512 KB — e o que a separa de um .bin de jogo. */
const BIOS_SIZE = 512 * 1024;

export type DroppedKind = 'bios' | 'exe' | 'disc';

function element<T extends HTMLElement>(id: string): T {
  const found = document.getElementById(id);
  if (!found) throw new Error(`Elemento #${id} nao existe no HTML.`);
  return found as T;
}

/** Nomes legiveis para `KeyboardEvent.code`, que e cru demais para a UI. */
function prettyKey(code: string | null): string {
  if (!code) return '—';
  return code
    .replace(/^Key/, '')
    .replace(/^Digit/, '')
    .replace(/^Arrow/, '')
    .replace(/^Numpad/, 'Num ')
    .replace(/Left$/, ' Esq.')
    .replace(/Right$/, ' Dir.');
}

export class Ui {
  readonly canvas = element<HTMLCanvasElement>('screen');
  readonly stage = element<HTMLElement>('stage');
  private readonly dropzone = element<HTMLElement>('dropzone');
  private readonly onboarding = element<HTMLElement>('onboarding');
  private readonly toast = element<HTMLElement>('toast');
  private readonly fileInput = element<HTMLInputElement>('file-input');
  private readonly diagnosticsList = element<HTMLElement>('diagnostics');
  private readonly keysDialog = element<HTMLDialogElement>('keys-dialog');
  private readonly keysList = element<HTMLElement>('keys-list');

  readonly buttons = {
    run: element<HTMLButtonElement>('btn-run'),
    reset: element<HTMLButtonElement>('btn-reset'),
    fast: element<HTMLButtonElement>('btn-fast'),
    mute: element<HTMLButtonElement>('btn-mute'),
    keys: element<HTMLButtonElement>('btn-keys'),
    pickBios: element<HTMLButtonElement>('pick-bios'),
    pickExe: element<HTMLButtonElement>('pick-exe'),
    pickDisc: element<HTMLButtonElement>('pick-disc'),
    keysReset: element<HTMLButtonElement>('keys-reset'),
  };

  readonly selects = {
    region: element<HTMLSelectElement>('select-region'),
    scaling: element<HTMLSelectElement>('select-scaling'),
    volume: element<HTMLInputElement>('volume'),
  };

  private readonly chips: Record<ChipName, HTMLElement> = {
    bios: element('chip-bios'),
    disc: element('chip-disc'),
    pad: element('chip-pad'),
    audio: element('chip-audio'),
  };

  private readonly fpsChip = element<HTMLElement>('chip-fps');
  private readonly versionLabel = element<HTMLElement>('version');
  private toastTimer: number | undefined;
  /** Resolvido pelo change do input de arquivo aberto por `pickFile`. */
  private pendingPick: ((file: File | null) => void) | null = null;

  constructor() {
    this.fileInput.addEventListener('change', () => {
      const file = this.fileInput.files?.[0] ?? null;
      // Zerar o valor permite escolher o mesmo arquivo duas vezes seguidas.
      this.fileInput.value = '';
      this.pendingPick?.(file);
      this.pendingPick = null;
    });
  }

  setChip(name: ChipName, state: ChipState, label?: string): void {
    const chip = this.chips[name];
    chip.dataset['state'] = state;
    if (label) chip.textContent = label;
  }

  setFps(fps: number | null): void {
    this.fpsChip.textContent = fps === null ? '— fps' : `${fps.toFixed(0)} fps`;
  }

  setVersion(version: string): void {
    this.versionLabel.textContent = `psx-core ${version}`;
  }

  setOnboardingVisible(visible: boolean): void {
    this.onboarding.hidden = !visible;
  }

  setRunning(running: boolean): void {
    this.buttons.run.textContent = running ? 'Pausar' : 'Iniciar';
    this.buttons.run.setAttribute('aria-pressed', String(running));
  }

  setControlsEnabled(enabled: boolean): void {
    this.buttons.run.disabled = !enabled;
    this.buttons.reset.disabled = !enabled;
    this.buttons.fast.disabled = !enabled;
  }

  notify(message: string, kind: 'info' | 'error' = 'info'): void {
    this.toast.textContent = message;
    this.toast.dataset['kind'] = kind;
    this.toast.hidden = false;
    window.clearTimeout(this.toastTimer);
    this.toastTimer = window.setTimeout(
      () => {
        this.toast.hidden = true;
      },
      kind === 'error' ? 6000 : 3000,
    );
  }

  /** Abre o seletor de arquivos e resolve com o escolhido (ou `null`). */
  pickFile(accept: string): Promise<File | null> {
    this.fileInput.accept = accept;
    return new Promise((resolve) => {
      this.pendingPick = resolve;
      this.fileInput.click();
    });
  }

  /**
   * Liga o drag-and-drop na janela inteira e classifica o arquivo pela
   * extensao — a BIOS e o caso padrao porque e o primeiro arquivo que todo
   * usuario precisa carregar.
   *
   * Soltar varios arquivos de uma vez e o caminho de um jogo em CUE+BIN: a
   * folha sozinha nao basta, e o nome que ela declara quase nunca bate com o
   * arquivo baixado.
   */
  onFilesDropped(handler: (files: File[]) => void): void {
    let depth = 0;

    const show = (visible: boolean) => {
      this.dropzone.hidden = !visible;
    };

    window.addEventListener('dragenter', (event) => {
      event.preventDefault();
      // dragenter/dragleave disparam para cada elemento filho: contamos a
      // profundidade para nao piscar o overlay ao cruzar as bordas internas.
      depth++;
      show(true);
    });
    window.addEventListener('dragover', (event) => {
      event.preventDefault();
      if (event.dataTransfer) event.dataTransfer.dropEffect = 'copy';
    });
    window.addEventListener('dragleave', (event) => {
      event.preventDefault();
      depth = Math.max(0, depth - 1);
      if (depth === 0) show(false);
    });
    window.addEventListener('drop', (event) => {
      event.preventDefault();
      depth = 0;
      show(false);
      const files = Array.from(event.dataTransfer?.files ?? []);
      if (files.length > 0) handler(files);
    });
  }

  /** Classifica um arquivo pelo nome e, quando disponivel, pelo tamanho. */
  static classify(name: string, size?: number): DroppedKind {
    return classify(name, size);
  }

  renderDiagnostics(values: Record<string, number>): void {
    const labels: Record<string, string> = {
      gteUnimplemented: 'GTE não implementado',
      gpuUnhandled: 'GPU não tratado',
      cdromUnimplemented: 'CD-ROM não implementado',
      busUnhandledReads: 'Leituras sem destino',
      busUnhandledWrites: 'Escritas sem destino',
    };
    this.diagnosticsList.replaceChildren(
      ...Object.entries(values).map(([key, value]) => {
        const row = document.createElement('div');
        const term = document.createElement('dt');
        term.textContent = labels[key] ?? key;
        const definition = document.createElement('dd');
        definition.textContent = value.toLocaleString('pt-BR');
        definition.dataset['nonzero'] = String(value > 0);
        row.append(term, definition);
        return row;
      }),
    );
  }

  /** Desenha a lista de teclas; `onChange` avisa quem persiste o mapa. */
  renderKeymap(input: Input, onChange: () => void): void {
    this.keysList.replaceChildren(
      ...input.bindings().map(([button, code]) => {
        const item = document.createElement('li');
        const label = document.createElement('span');
        label.textContent = button;
        const trigger = document.createElement('button');
        trigger.type = 'button';
        trigger.textContent = prettyKey(code);
        trigger.addEventListener('click', () => {
          trigger.textContent = 'pressione…';
          input.capture(button as ButtonName, () => {
            this.renderKeymap(input, onChange);
            onChange();
          });
        });
        item.append(label, trigger);
        return item;
      }),
    );
    // A lista tem que cobrir os 16 botoes, mesmo os sem ligacao.
    console.assert(this.keysList.children.length === BUTTONS.length);
  }

  openKeymap(): void {
    this.keysDialog.showModal();
  }
}

/**
 * Decide o que um arquivo e.
 *
 * A extensao sozinha nao resolve: uma BIOS chama `SCPH1001.BIN` e uma imagem
 * de jogo chama `jogo.bin`. O que separa os dois com seguranca e o tamanho —
 * uma BIOS de PSX tem exatamente 512 KB, e nenhuma imagem de disco util e tao
 * pequena.
 */
function classify(name: string, size?: number): DroppedKind {
  const lower = name.toLowerCase();
  if (EXE_EXTENSIONS.some((extension) => lower.endsWith(extension))) return 'exe';

  if (lower.endsWith('.bin')) {
    if (size === undefined) return 'bios';
    return size === BIOS_SIZE ? 'bios' : 'disc';
  }

  if (DISC_EXTENSIONS.some((extension) => lower.endsWith(extension))) return 'disc';
  return 'bios';
}
