/**
 * Blit do framebuffer da GPU emulada para o canvas via WebGL2.
 *
 * O core entrega RGBA8 ja no formato do display (320x240 ate 640x480). Aqui
 * so subimos a textura e desenhamos um quad de tela cheia com letterbox 4:3 —
 * nenhuma decisao de emulacao mora neste arquivo.
 */

const VERTEX_SHADER = `#version 300 es
// Triangulo unico que cobre a tela: mais barato que um quad de dois
// triangulos e evita a costura na diagonal.
out vec2 v_uv;
void main() {
  vec2 position = vec2(float((gl_VertexID << 1) & 2), float(gl_VertexID & 2));
  // O framebuffer do PSX tem a origem no topo, o clip space do GL embaixo.
  v_uv = vec2(position.x, 1.0 - position.y);
  gl_Position = vec4(position * 2.0 - 1.0, 0.0, 1.0);
}`;

const FRAGMENT_SHADER = `#version 300 es
precision mediump float;
in vec2 v_uv;
uniform sampler2D u_frame;
out vec4 outColor;
void main() {
  outColor = vec4(texture(u_frame, v_uv).rgb, 1.0);
}`;

/** Proporcao de tela do PSX, independente da resolucao interna. */
const ASPECT = 4 / 3;

export type ScalingMode = 'sharp' | 'smooth';

export class Renderer {
  private readonly gl: WebGL2RenderingContext;
  private readonly texture: WebGLTexture;
  private textureWidth = 0;
  private textureHeight = 0;

  private constructor(
    private readonly canvas: HTMLCanvasElement,
    gl: WebGL2RenderingContext,
    texture: WebGLTexture,
  ) {
    this.gl = gl;
    this.texture = texture;
  }

  static create(canvas: HTMLCanvasElement, scaling: ScalingMode = 'sharp'): Renderer {
    const gl = canvas.getContext('webgl2', {
      alpha: false,
      antialias: false,
      depth: false,
      stencil: false,
      // Sem isto o navegador pode limpar o buffer entre frames em alguns
      // compositores, causando flicker quando o emulador esta pausado.
      preserveDrawingBuffer: true,
      powerPreference: 'high-performance',
    });
    if (!gl) {
      throw new Error('WebGL2 nao esta disponivel neste navegador.');
    }

    const program = linkProgram(gl, VERTEX_SHADER, FRAGMENT_SHADER);
    gl.useProgram(program);
    gl.uniform1i(gl.getUniformLocation(program, 'u_frame'), 0);

    // WebGL2 exige um VAO ligado mesmo quando o vertex shader nao le atributos.
    gl.bindVertexArray(gl.createVertexArray());

    const texture = gl.createTexture();
    gl.activeTexture(gl.TEXTURE0);
    gl.bindTexture(gl.TEXTURE_2D, texture);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);

    const renderer = new Renderer(canvas, gl, texture);
    renderer.setScaling(scaling);
    return renderer;
  }

  setScaling(mode: ScalingMode): void {
    const { gl } = this;
    const filter = mode === 'sharp' ? gl.NEAREST : gl.LINEAR;
    gl.bindTexture(gl.TEXTURE_2D, this.texture);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, filter);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, filter);
  }

  /**
   * Desenha um frame.
   *
   * `pixels` e uma view sobre a memoria linear do WASM e pode ser invalidada
   * a qualquer crescimento dela — por isso quem chama remonta a view a cada
   * frame em vez de o renderer guardar a referencia.
   */
  draw(pixels: Uint8Array, width: number, height: number): void {
    if (width === 0 || height === 0) return;
    const { gl } = this;
    const expected = width * height * 4;
    if (pixels.length < expected) return;

    this.resizeToDisplay();
    gl.activeTexture(gl.TEXTURE0);
    gl.bindTexture(gl.TEXTURE_2D, this.texture);
    // Cada linha do framebuffer e contigua; sem isto o GL assume alinhamento 4
    // e larguras impares do PSX (ex.: 368) saem tortas.
    gl.pixelStorei(gl.UNPACK_ALIGNMENT, 1);

    const data = pixels.subarray(0, expected);
    if (width !== this.textureWidth || height !== this.textureHeight) {
      gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, width, height, 0, gl.RGBA, gl.UNSIGNED_BYTE, data);
      this.textureWidth = width;
      this.textureHeight = height;
    } else {
      gl.texSubImage2D(gl.TEXTURE_2D, 0, 0, 0, width, height, gl.RGBA, gl.UNSIGNED_BYTE, data);
    }

    this.applyLetterbox();
    gl.drawArrays(gl.TRIANGLES, 0, 3);
  }

  /** Pinta o canvas de preto — usado no reset e enquanto nao ha BIOS. */
  clear(): void {
    const { gl } = this;
    this.resizeToDisplay();
    gl.viewport(0, 0, this.canvas.width, this.canvas.height);
    gl.clearColor(0, 0, 0, 1);
    gl.clear(gl.COLOR_BUFFER_BIT);
  }

  /** Acompanha o tamanho CSS do canvas, respeitando a densidade da tela. */
  private resizeToDisplay(): void {
    const ratio = Math.min(window.devicePixelRatio || 1, 2);
    const width = Math.max(1, Math.round(this.canvas.clientWidth * ratio));
    const height = Math.max(1, Math.round(this.canvas.clientHeight * ratio));
    if (this.canvas.width !== width || this.canvas.height !== height) {
      this.canvas.width = width;
      this.canvas.height = height;
    }
  }

  /**
   * Centraliza a imagem em 4:3 dentro do canvas. Fazemos no viewport em vez do
   * CSS para que o canvas possa ocupar a area toda sem distorcer a imagem.
   */
  private applyLetterbox(): void {
    const { gl, canvas } = this;
    const { width, height } = canvas;
    let viewWidth = width;
    let viewHeight = Math.round(width / ASPECT);
    if (viewHeight > height) {
      viewHeight = height;
      viewWidth = Math.round(height * ASPECT);
    }
    const x = Math.floor((width - viewWidth) / 2);
    const y = Math.floor((height - viewHeight) / 2);

    // Limpa a moldura antes de desenhar, senao o letterbox mantem lixo do
    // frame anterior quando a resolucao interna muda.
    gl.viewport(0, 0, width, height);
    gl.clearColor(0, 0, 0, 1);
    gl.clear(gl.COLOR_BUFFER_BIT);
    gl.viewport(x, y, viewWidth, viewHeight);
  }
}

function linkProgram(gl: WebGL2RenderingContext, vertexSource: string, fragmentSource: string): WebGLProgram {
  const program = gl.createProgram();
  const vertex = compileShader(gl, gl.VERTEX_SHADER, vertexSource);
  const fragment = compileShader(gl, gl.FRAGMENT_SHADER, fragmentSource);
  gl.attachShader(program, vertex);
  gl.attachShader(program, fragment);
  gl.linkProgram(program);
  if (!gl.getProgramParameter(program, gl.LINK_STATUS)) {
    const log = gl.getProgramInfoLog(program);
    throw new Error(`Falha ao linkar o programa WebGL: ${log}`);
  }
  // Ja linkados: os objetos de shader nao servem mais para nada.
  gl.deleteShader(vertex);
  gl.deleteShader(fragment);
  return program;
}

function compileShader(gl: WebGL2RenderingContext, type: number, source: string): WebGLShader {
  const shader = gl.createShader(type);
  if (!shader) throw new Error('Nao foi possivel criar o shader.');
  gl.shaderSource(shader, source);
  gl.compileShader(shader);
  if (!gl.getShaderParameter(shader, gl.COMPILE_STATUS)) {
    const log = gl.getShaderInfoLog(shader);
    throw new Error(`Falha ao compilar o shader: ${log}`);
  }
  return shader;
}
