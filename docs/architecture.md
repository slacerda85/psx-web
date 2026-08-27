# Arquitetura

Documento vivo. O plano original, com o roadmap por fases e a especificação
dos agentes, está em [plano-emulador-psx-web.md](plano-emulador-psx-web.md).

## Princípio central: o core não conhece o navegador

`psx-core` é lógica pura — sem dependências, sem `unsafe`, sem I/O e sem
`wasm-bindgen`. Ele recebe bytes, executa ciclos e devolve buffers. Quem lida
com arquivos, telas e alto-falantes é o embedder.

Isso não é purismo: é o que permite rodar 191 testes de emulação em poucos milissegundos com
`cargo test`, sem navegador, sem headless browser e sem harness. Um bug de
GTE é reproduzido num teste unitário de dez linhas, não num jogo travando.

```
┌──────────────────────────── navegador ────────────────────────────┐
│                                                                   │
│  main.ts ──┬── renderer.ts ── WebGL2 ── <canvas>                   │
│            ├── audio.ts ───── AudioWorklet ── alto-falante        │
│            ├── input.ts ───── teclado / Gamepad API               │
│            ├── ui.ts ──────── DOM, drag-and-drop, diagnóstico     │
│            └── storage.ts ─── IndexedDB (BIOS, teclas, memcards)  │
│                    │                                              │
│                    │ Emulator (wasm-bindgen)                      │
│  ┌─────────────────▼───────────────────────────────────────────┐  │
│  │ psx-wasm — camada fina, só tradução de tipos                │  │
│  └─────────────────┬───────────────────────────────────────────┘  │
│                    │                                              │
│  ┌─────────────────▼───────────────────────────────────────────┐  │
│  │ psx-core                                                    │  │
│  │                                                             │  │
│  │   System ── orquestra o frame                               │  │
│  │     ├── Cpu (R3000A + COP0)                                 │  │
│  │     ├── Gte (COP2)                                          │  │
│  │     └── Bus ─┬── Ram / Bios / Scratchpad                    │  │
│  │              ├── Gpu (VRAM + rasterizador)                  │  │
│  │              ├── Spu, Cdrom, Sio, Timers                    │  │
│  │              ├── Dma (7 canais)                             │  │
│  │              └── IrqController                              │  │
│  └─────────────────────────────────────────────────────────────┘  │
└───────────────────────────────────────────────────────────────────┘
```

A fonte da verdade para comportamento de hardware é o
[PSX-SPX](https://psx-spx.consoledev.net/). Cada módulo cita, no topo, a seção
que implementa — e onde o escopo atual para.

## O laço de um frame

`System::run_frame` executa `Region::cycles_per_frame` ciclos de CPU
(33.8688 MHz ÷ 60 no NTSC, ÷ 50 no PAL) e devolve `FrameStats`. Dentro do
laço, cada instrução executada avança os periféricos pelo mesmo número de
ciclos: GPU, timers, CD-ROM e SPU são acertados a partir do relógio da CPU, e
não de um agendador de eventos separado.

O clock de 33 868 800 Hz é exatamente 44 100 × 768 — por isso o SPU produz uma
amostra a cada 768 ciclos, sem acumulador fracionário.

O frontend não confia no `requestAnimationFrame` para o ritmo: ele acumula o
tempo real decorrido e roda frames enquanto houver dívida, com teto de quatro
frames por callback. Sem esse teto, uma aba em segundo plano voltaria com
segundos de dívida e o emulador entraria em espiral tentando recuperá-la.

## Contrato entre core e frontend

Quatro pontos, definidos em [`crates/psx-wasm/src/lib.rs`](../crates/psx-wasm/src/lib.rs):

1. **`new Emulator(biosBytes)`** — a BIOS vem do usuário, sempre.
2. **`runFrame()`** a cada frame de vídeo.
3. **`framebufferPtr()` + `framebufferLength()`** descrevem uma região RGBA8
   dentro da memória linear do WASM. O JavaScript monta um `Uint8Array` sobre
   ela: zero cópia.
4. **`drainAudio(buffer)`** entrega amostras f32 estéreo intercaladas.

O ponto 3 tem uma armadilha que vale registrar: **a view precisa ser remontada
a cada frame**. Qualquer crescimento da memória linear do WASM desanexa o
`ArrayBuffer` anterior, e uma view guardada entre frames silenciosamente vira
lixo. Devolver o ponteiro em vez de uma view mantém `psx-wasm` livre de
`unsafe` — quem constrói a view é o JavaScript, que não tem esse conceito.

## Vídeo

O rasterizador é por software, dentro do core, escrevendo em VRAM de
1024×512×16 bits. O core converte a janela de display ativa para RGBA8 e o
frontend faz apenas o blit.

Essa divisão é deliberada. Um backend WebGL de alto nível (a alternativa
avaliada na seção 1 do plano) exigiria reimplementar as regras de precisão do
PSX — coordenadas inteiras, texturas afins sem correção de perspectiva,
dithering 24→15 bits — em cima de uma GPU que faz tudo diferente. Rasterizando
por software, essas regras saem de graça e a compatibilidade sobe.

O `Renderer` faz o letterbox 4:3 no viewport do WebGL, não no CSS, para que o
canvas ocupe toda a área disponível sem distorcer a imagem. `UNPACK_ALIGNMENT`
é fixado em 1: larguras ímpares do PSX (368, por exemplo) sairiam tortas com o
alinhamento padrão de 4.

## Áudio

O SPU gera a 44 100 Hz; o `AudioContext` do dispositivo pode acabar em 48 000.
O `AudioWorkletProcessor` reamostra linearmente pela razão entre as duas taxas,
em vez de assumir que o pedido de 44 100 foi atendido.

O processador vive numa URL de `Blob` gerada em tempo de execução. Um worklet
precisa ser carregado de uma URL própria, e um Blob evita depender do layout de
assets do bundler e da `base` relativa no deploy — o código do worklet continua
versionado junto do resto do frontend, em `audio-worklet.ts`.

A fila do worklet tem teto: se o main thread produzir mais rápido do que a
saída consome, descartar o bloco mais antigo é melhor do que deixar a latência
crescer indefinidamente.

## CD-ROM

O drive lê ISO (setores de 2048 B), BIN cru e BIN/CUE (2352 B). O formato é
deduzido do padrão de sincronismo no início do arquivo, e não da extensão:
imagens circulam renomeadas o tempo todo.

O offset dos dados dentro de um setor cru sai do byte de modo do próprio
setor, não de configuração — discos de PSX misturam Mode 1 (dados em +16) e
Mode 2 Form 1 (dados em +24) no mesmo disco.

O core não abre arquivos. Num CUE, quem localiza o binário referenciado é o
embedder: o nome dentro da folha quase nunca sobrevive ao download.

A imagem inteira fica em RAM hoje. Para um jogo de 700 MB isso é muito, e a
evolução natural é entregar setores sob demanda — a interface de leitura já é
por LBA justamente para permitir essa troca sem tocar no controlador.

## Persistência

Tudo em IndexedDB, não em `localStorage`: a BIOS tem 512 KB e cada memory card
tem 128 KB, binários que `localStorage` não guarda e cuja soma estoura a cota
dele. Nada sai do navegador.

## Cross-origin isolation

Os headers `COOP: same-origin` e `COEP: require-corp` já estão ligados no dev
server e no preview, embora o core ainda seja single-thread. Ligá-los depois
significaria mudar o deploy inteiro; ligá-los agora deixa `SharedArrayBuffer`
disponível para a Fase 6 (threads).

**GitHub Pages não permite headers customizados.** O emulador funciona lá do
mesmo jeito, porque nada hoje depende de isolamento — mas a Fase 6 vai exigir
um host que permita configurá-los (Cloudflare Pages, Netlify, Vercel).

## Decisões registradas

| Decisão | Motivo |
| --- | --- |
| Rust + WASM, não JS | Determinismo de inteiros e performance previsível no interpretador |
| Rasterizador por software | Precisão do PSX é mais fácil de acertar na CPU do que de emular numa GPU moderna |
| `forbid(unsafe_code)` nos dois crates | Um bug de emulação deve dar resultado errado, nunca corromper memória |
| Zero dependências no core | Superfície de auditoria mínima e compilação rápida |
| Ponteiro em vez de view no framebuffer | Mantém `psx-wasm` seguro sem abrir mão do zero-cópia |
| TypeScript sem framework | O DOM da UI é pequeno; um framework custaria mais bytes do que o próprio emulador |
| IndexedDB para tudo | Único armazenamento local com cota e tipo adequados a binários |

## O que ainda não existe

Os contadores de `Diagnostics` expõem isso em tempo real na UI, em vez de
deixar o emulador falhar em silêncio:

- **SPU** — os registradores, a RAM e o DMA funcionam, mas as 24 vozes ainda
  não são mixadas: não há ADPCM, ADSR nem reverb.
- **GTE** — o banco de registradores, os espelhos e as flags estão completos,
  mas `Gte::execute` ainda não implementa **nenhum** dos comandos: ele conta o
  opcode e devolve os ciclos. É o que impede um jogo 3D de desenhar qualquer
  coisa, mesmo carregando e executando normalmente.
- **CD-ROM** — lê ISO, BIN cru e BIN/CUE. Falta CD-DA (faixas de áudio) e
  XA-ADPCM; CHD não é suportado.
- **SIO0** — o controller digital está completo; DualShock (modo analógico,
  config mode, rumble) e o protocolo de memory card não.
- **GPU** — o rasterizador cobre polígonos, linhas, retângulos, mask bits e os
  quatro modos de semi-transparência. O contador `gpuUnhandled` acusa qualquer
  comando GP0/GP1 que ainda caia no caso padrão.

Cada um desses tem um agente responsável em [`.claude/agents/`](../.claude/agents/).
