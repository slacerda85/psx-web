/**
 * Saida de audio via `AudioWorklet`.
 *
 * O core produz i16 estereo intercalado a 44100 Hz; `Emulator.drainAudio` ja
 * converte para f32 normalizado. Aqui so empurramos os blocos para o worklet,
 * que faz a reamostragem para a taxa real do `AudioContext`.
 */

import { PROCESSOR_NAME, processorUrl } from './audio-worklet';

/** Taxa nativa do SPU (PSX-SPX, "SPU Overview"). */
export const SPU_SAMPLE_RATE = 44100;

/** Amostras (nao frames) pedidas ao core por chamada. */
const DRAIN_CAPACITY = 4096;

export class Audio {
  private context: AudioContext | null = null;
  private node: AudioWorkletNode | null = null;
  private gain: GainNode | null = null;
  private readonly scratch = new Float32Array(DRAIN_CAPACITY);
  private volume = 0.8;
  private muted = false;

  /**
   * Cria o grafo de audio. Precisa ser chamado a partir de um gesto do
   * usuario: navegadores criam o `AudioContext` suspenso caso contrario.
   */
  async start(): Promise<void> {
    if (this.context) {
      await this.resume();
      return;
    }

    // Pedir 44100 evita reamostragem quando o dispositivo aceita; quando nao
    // aceita, o worklet compensa usando `sampleRate` real do contexto.
    let context: AudioContext;
    try {
      context = new AudioContext({ sampleRate: SPU_SAMPLE_RATE, latencyHint: 'interactive' });
    } catch {
      context = new AudioContext({ latencyHint: 'interactive' });
    }

    const url = processorUrl();
    try {
      await context.audioWorklet.addModule(url);
    } finally {
      // A URL de Blob so precisa sobreviver ate o modulo ser lido.
      URL.revokeObjectURL(url);
    }

    const node = new AudioWorkletNode(context, PROCESSOR_NAME, {
      numberOfInputs: 0,
      numberOfOutputs: 1,
      outputChannelCount: [2],
      processorOptions: { sourceRate: SPU_SAMPLE_RATE },
    });
    const gain = context.createGain();
    gain.gain.value = this.muted ? 0 : this.volume;
    node.connect(gain).connect(context.destination);

    this.context = context;
    this.node = node;
    this.gain = gain;
    await this.resume();
  }

  async resume(): Promise<void> {
    if (this.context?.state === 'suspended') await this.context.resume();
  }

  async suspend(): Promise<void> {
    if (this.context?.state === 'running') await this.context.suspend();
  }

  get ready(): boolean {
    return this.node !== null;
  }

  get sampleRate(): number | null {
    return this.context?.sampleRate ?? null;
  }

  /**
   * Drena o emulador e entrega o bloco ao worklet.
   *
   * `drain` recebe o buffer de rascunho e devolve quantas amostras escreveu —
   * a mesma assinatura de `Emulator.drainAudio`, para nao acoplar este modulo
   * ao tipo gerado pelo wasm-bindgen.
   */
  pump(drain: (out: Float32Array) => number): void {
    const node = this.node;
    if (!node) return;
    const written = drain(this.scratch);
    if (written === 0) return;
    // O worklet vive em outra thread: a copia e obrigatoria, senao o proximo
    // `pump` sobrescreveria o bloco enquanto ele ainda toca.
    const block = this.scratch.slice(0, written);
    node.port.postMessage(block, [block.buffer]);
  }

  /** Descarta o que estiver na fila — usado em reset e ao pausar. */
  flush(): void {
    this.node?.port.postMessage('flush');
  }

  setVolume(volume: number): void {
    this.volume = Math.min(Math.max(volume, 0), 1);
    this.applyGain();
  }

  setMuted(muted: boolean): void {
    this.muted = muted;
    this.applyGain();
  }

  private applyGain(): void {
    if (!this.gain || !this.context) return;
    // Rampa curta: mudar `value` direto produz clique audivel.
    const target = this.muted ? 0 : this.volume;
    this.gain.gain.setTargetAtTime(target, this.context.currentTime, 0.01);
  }

  async close(): Promise<void> {
    this.node?.disconnect();
    this.gain?.disconnect();
    await this.context?.close();
    this.context = null;
    this.node = null;
    this.gain = null;
  }
}
