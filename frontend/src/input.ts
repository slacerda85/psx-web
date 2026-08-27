/**
 * Entrada: teclado e gamepad -> mascara de 16 bits ativo-alta.
 *
 * A ordem dos bits e a do protocolo SIO0 (PSX-SPX, "Controllers and Memory
 * Cards"); `Emulator.setButtons` inverte para o formato ativo-baixo do
 * hardware, entao aqui bit 1 significa *pressionado*.
 */

export const BUTTONS = [
  'Select',
  'L3',
  'R3',
  'Start',
  'Up',
  'Right',
  'Down',
  'Left',
  'L2',
  'R2',
  'L1',
  'R1',
  'Triangle',
  'Circle',
  'Cross',
  'Square',
] as const;

export type ButtonName = (typeof BUTTONS)[number];

export type KeyMap = Record<string, ButtonName>;

/** Mapa padrao, pensado para teclado ABNT/US sem exigir numpad. */
export const DEFAULT_KEYMAP: KeyMap = {
  ArrowUp: 'Up',
  ArrowDown: 'Down',
  ArrowLeft: 'Left',
  ArrowRight: 'Right',
  KeyW: 'Triangle',
  KeyS: 'Cross',
  KeyA: 'Square',
  KeyD: 'Circle',
  Enter: 'Start',
  ShiftRight: 'Select',
  KeyQ: 'L1',
  KeyE: 'R1',
  KeyZ: 'L2',
  KeyC: 'R2',
  KeyF: 'L3',
  KeyG: 'R3',
};

const BIT: Record<ButtonName, number> = Object.fromEntries(
  BUTTONS.map((name, index) => [name, 1 << index]),
) as Record<ButtonName, number>;

/**
 * Ordem dos botoes no Standard Gamepad do navegador. `null` marca indices que
 * nao tem equivalente no PSX (Home/Guide).
 */
const GAMEPAD_BUTTONS: (ButtonName | null)[] = [
  'Cross',
  'Circle',
  'Square',
  'Triangle',
  'L1',
  'R1',
  'L2',
  'R2',
  'Select',
  'Start',
  'L3',
  'R3',
  'Up',
  'Down',
  'Left',
  'Right',
  null,
];

/** Alem de qual valor um eixo analogico conta como direcional pressionado. */
const AXIS_DEADZONE = 0.5;

export class Input {
  private keymap: KeyMap;
  private readonly held = new Set<string>();
  /** Quando nao-nulo, a proxima tecla pressionada e atribuida a este botao. */
  private capturing: ButtonName | null = null;
  private onCaptured: ((button: ButtonName, code: string) => void) | null = null;

  constructor(keymap: KeyMap = { ...DEFAULT_KEYMAP }) {
    this.keymap = keymap;
  }

  attach(target: Window = window): () => void {
    const down = (event: KeyboardEvent) => {
      if (this.capturing) {
        event.preventDefault();
        const button = this.capturing;
        this.capturing = null;
        if (event.code !== 'Escape') {
          this.rebind(button, event.code);
          this.onCaptured?.(button, event.code);
        }
        return;
      }
      if (this.keymap[event.code]) {
        event.preventDefault();
        this.held.add(event.code);
      }
    };
    const up = (event: KeyboardEvent) => {
      this.held.delete(event.code);
    };
    // Sem isto, trocar de aba com um botao pressionado deixa o botao "colado".
    const blur = () => this.held.clear();

    target.addEventListener('keydown', down);
    target.addEventListener('keyup', up);
    target.addEventListener('blur', blur);
    return () => {
      target.removeEventListener('keydown', down);
      target.removeEventListener('keyup', up);
      target.removeEventListener('blur', blur);
    };
  }

  /** Mascara ativo-alta combinando teclado e o primeiro gamepad conectado. */
  mask(): number {
    let mask = 0;
    for (const code of this.held) {
      const button = this.keymap[code];
      if (button) mask |= BIT[button];
    }
    return mask | this.gamepadMask();
  }

  private gamepadMask(): number {
    // `getGamepads` devolve um snapshot novo a cada chamada: nao da para
    // guardar a referencia entre frames.
    const pads = navigator.getGamepads?.() ?? [];
    const pad = pads.find((candidate) => candidate?.connected);
    if (!pad) return 0;

    let mask = 0;
    for (let index = 0; index < pad.buttons.length; index++) {
      const name = GAMEPAD_BUTTONS[index];
      if (name && pad.buttons[index]?.pressed) mask |= BIT[name];
    }
    const [x = 0, y = 0] = pad.axes;
    if (x < -AXIS_DEADZONE) mask |= BIT.Left;
    if (x > AXIS_DEADZONE) mask |= BIT.Right;
    if (y < -AXIS_DEADZONE) mask |= BIT.Up;
    if (y > AXIS_DEADZONE) mask |= BIT.Down;
    return mask;
  }

  /** Liga `code` a `button`, removendo qualquer ligacao anterior das duas pontas. */
  rebind(button: ButtonName, code: string): void {
    for (const [existing, bound] of Object.entries(this.keymap)) {
      if (bound === button) delete this.keymap[existing];
    }
    this.keymap[code] = button;
    this.held.clear();
  }

  /** Arma a captura da proxima tecla. `Escape` cancela. */
  capture(button: ButtonName, onCaptured: (button: ButtonName, code: string) => void): void {
    this.capturing = button;
    this.onCaptured = onCaptured;
  }

  bindings(): ReadonlyArray<[ButtonName, string | null]> {
    const reverse = new Map<ButtonName, string>();
    for (const [code, button] of Object.entries(this.keymap)) {
      if (button && !reverse.has(button)) reverse.set(button, code);
    }
    return BUTTONS.map((button) => [button, reverse.get(button) ?? null]);
  }

  toJSON(): KeyMap {
    return { ...this.keymap };
  }

  load(keymap: KeyMap): void {
    this.keymap = { ...keymap };
    this.held.clear();
  }
}
