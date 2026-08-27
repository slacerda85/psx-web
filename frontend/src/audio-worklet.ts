/**
 * Codigo do `AudioWorkletProcessor`, mantido como string.
 *
 * Um worklet precisa ser carregado de uma URL propria. Usar um Blob evita
 * depender do layout de assets do bundler e de `base` relativa no deploy —
 * o processador continua versionado junto do resto do frontend.
 *
 * O SPU produz 44100 Hz; o `AudioContext` pode acabar em 48000. O processador
 * reamostra linearmente pela razao entre as duas taxas em vez de assumir que
 * o pedido de 44100 foi atendido.
 */
export const PROCESSOR_NAME = 'psx-spu';

const SOURCE = /* js */ `
// Alvo de latencia: abaixo disto pedimos ao main thread para nao segurar
// amostras; acima, deixamos a fila drenar. ~50 ms a 44.1 kHz.
const TARGET_FRAMES = 2048;
const MAX_FRAMES = 8192;

class PsxSpuProcessor extends AudioWorkletProcessor {
  constructor(options) {
    super();
    this.sourceRate = options.processorOptions.sourceRate;
    this.ratio = this.sourceRate / sampleRate;
    // Fila de blocos estereo intercalados vindos do main thread.
    this.queue = [];
    this.queued = 0;
    this.offset = 0;
    this.position = 0;
    this.lastLeft = 0;
    this.lastRight = 0;
    this.underruns = 0;

    this.port.onmessage = (event) => {
      const data = event.data;
      if (data === 'flush') {
        this.queue.length = 0;
        this.queued = 0;
        this.offset = 0;
        this.position = 0;
        return;
      }
      this.queue.push(data);
      this.queued += data.length >> 1;
      // Se o main thread produziu mais rapido do que consumimos por muito
      // tempo, descartar o mais antigo e melhor do que crescer a latencia.
      while (this.queued > MAX_FRAMES && this.queue.length > 1) {
        const dropped = this.queue.shift();
        this.queued -= dropped.length >> 1;
        this.offset = 0;
      }
    };
  }

  // Consome um frame estereo da fila. Devolve false em underrun.
  nextFrame() {
    while (this.queue.length > 0) {
      const block = this.queue[0];
      if (this.offset + 1 < block.length) {
        this.lastLeft = block[this.offset];
        this.lastRight = block[this.offset + 1];
        this.offset += 2;
        this.queued--;
        return true;
      }
      this.queue.shift();
      this.offset = 0;
    }
    return false;
  }

  process(_inputs, outputs) {
    const output = outputs[0];
    if (!output || output.length === 0) return true;
    const left = output[0];
    const right = output.length > 1 ? output[1] : output[0];

    for (let i = 0; i < left.length; i++) {
      // Avanca na taxa da fonte; com ratio != 1 isto consome mais (ou menos)
      // de um frame por amostra de saida.
      this.position += this.ratio;
      let advanced = false;
      while (this.position >= 1) {
        this.position -= 1;
        if (!this.nextFrame()) {
          this.underruns++;
          this.position = 0;
          break;
        }
        advanced = true;
      }
      void advanced;
      left[i] = this.lastLeft;
      right[i] = this.lastRight;
    }
    return true;
  }

  static get parameterDescriptors() {
    return [];
  }
}

registerProcessor(${JSON.stringify(PROCESSOR_NAME)}, PsxSpuProcessor);
`;

/** URL de Blob com o processador, pronta para `audioWorklet.addModule`. */
export function processorUrl(): string {
  return URL.createObjectURL(new Blob([SOURCE], { type: 'application/javascript' }));
}
