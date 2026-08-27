# Plano Detalhado: Emulador PlayStation 1 (PSX) para Navegadores Web

**Projeto:** `psx-web-emulator`  
**Objetivo:** Emulador completo, compatível com BIOS originais, ISOs (BIN/CUE/ISO), Memory Cards (.mcd) e controles, rodando 100% no navegador.  
**Data:** 2026-08-23  
**Versão do Plano:** 1.0

---

## 1. Análise Comparativa das Abordagens

### 1.1 Three.js (WebGL de alto nível)

**Como funcionaria:**
- Core de emulação (CPU/GTE/etc.) separado (JS ou WASM).
- GPU emulada em High-Level Emulation (HLE): interpretar comandos GP0/GP1 e traduzir para `THREE.Mesh`, `THREE.BufferGeometry`, materials customizados com shaders que simulam affine texturing, dithering, vertex snapping, 15-bit color, etc.

**Prós:**
- Visual "PSX-style" fácil de obter com shaders (já existem projetos como PlayShader One).
- Bom para demos ou visualizadores de modelos TMD.

**Contras (decisivos):**
- Não é emulação de hardware. Jogos reais dependem de timing exato de DMA, GPUSTAT, VRAM layout (1024×512), texture windows, CLUT, etc. HLE quebra a maioria dos jogos comerciais.
- Overhead alto do scene graph do Three.js.
- Renderização **não é a mais leve**.
- Complexidade de mapeamento de comandos GPU → Three.js é maior do que fazer um software rasterizer ou hardware-accelerated rasterizer direto.
- Escalabilidade ruim para precisão ciclo-a-ciclo ou near-cycle.

**Conclusão:** Inadequado para um emulador real de jogos originais.

### 1.2 WebAssembly "Bare Metal"

**Como funcionaria:**
- Emulador escrito em linguagem de baixo nível (Rust recomendado) compilado para WASM.
- Emulação Low-Level (LLE) ou híbrida do hardware:
  - CPU MIPS R3000A (interpreter + eventual dynarec)
  - Coprocessor GTE
  - GPU (comandos + software/hardware rasterizer)
  - SPU, MDEC, CD-ROM controller, DMA, Timers, SIO, etc.
- Framebuffer (VRAM) é um `Uint8Array` / `ImageData` / texture WebGL.
- Blit extremamente leve: `gl.texSubImage2D` a cada frame (ou a cada scanline se necessário).
- Frontend JS mínimo apenas para UI, File API, Gamepad API e Web Audio.

**Prós:**
- Precisão alta possível (baseado em specs nocash PSX-SPX).
- Performance excelente (WASM é near-native).
- Renderização **mais leve possível** (apenas upload de texture 320×240~640×480).
- Escalável: dynarec futuro, multi-threading (SharedArrayBuffer + COOP/COEP), WebGPU, etc.
- Já existem referências maduras: RSX (Rust), nuPSX (Zig + WASM), ImLunaHey/emulators (Rust PS1), wasmpsx (C + Emscripten), etc.
- Código moderno, testável, safe (Rust).

**Contras:**
- Curva de aprendizado inicial da arquitetura PSX (mas bem documentada).
- Projeto grande (vários meses de trabalho focado).

**Conclusão:** **Escolhida**. É a única abordagem que atende simultaneamente simplicidade a longo prazo, enxutez, escalabilidade e renderização mais leve.

---

## 2. Stack Tecnológica Escolhida (Máxima Simplicidade + Leveza + Escalabilidade)

| Camada              | Tecnologia                          | Justificativa |
|---------------------|-------------------------------------|---------------|
| Core Emulation      | **Rust** (2021 edition+)            | Safety, performance, excelente suporte WASM, comunidade Emudev ativa |
| Compilação WASM     | `wasm-pack` + `wasm-bindgen`        | Padrão de fato, zero boilerplate desnecessário |
| Frontend            | **Vanilla TypeScript** + Vite       | Zero framework overhead, bundle mínimo |
| Rendering           | **WebGL2** puro (sem Three.js)      | Upload de framebuffer mais leve possível |
| Áudio               | Web Audio API (ScriptProcessor ou AudioWorklet) | Baixa latência |
| Persistência        | IndexedDB + File System Access API  | BIOS, saves, ISOs locais |
| Controles           | Gamepad API + Keyboard              | Nativo |
| Build / CI          | GitHub Actions + cargo + wasm-pack  | Automático |
| Testes              | `cargo test` + Playwright (E2E) + testes de hardware (Amidog, etc.) | |
| Versionamento       | Semantic Versioning + Conventional Commits | |

**Dependências externas mínimas:**
- `wasm-bindgen`
- `js-sys` / `web-sys` (apenas o necessário)
- Nenhuma lib 3D, nenhum framework UI pesado.

**Target de performance:**
- Full speed (60 FPS / 30 FPS conforme jogo) em Chrome/Firefox desktop moderno.
- Mobile: playable em mid-high end (com downscale).

---

## 3. Arquitetura de Alto Nível

```
┌─────────────────────────────────────────────────────────────┐
│                     Frontend (TypeScript)                   │
│  UI (load BIOS/ISO/MC) │ Canvas/WebGL │ Audio │ Input      │
└────────────┬──────────────────────┬─────────────────────────┘
             │ wasm-bindgen         │ SharedArrayBuffer (opt)
             ▼                      ▼
┌─────────────────────────────────────────────────────────────┐
│                     WASM Core (Rust)                        │
│  ┌─────────┐  ┌─────┐  ┌─────┐  ┌──────┐  ┌─────────────┐  │
│  │  CPU    │  │ GTE │  │ GPU │  │ SPU  │  │ CD-ROM      │  │
│  │ R3000A  │  │     │  │     │  │      │  │ Controller  │  │
│  └────┬────┘  └──┬──┘  └──┬──┘  └──┬───┘  └──────┬──────┘  │
│       │          │        │        │             │         │
│  ┌────▼──────────▼────────▼────────▼─────────────▼──────┐  │
│  │              Bus + DMA + Timers + Interrupts         │  │
│  └──────────────────────────────────────────────────────┘  │
│  Memory: 2MB Main RAM + 1MB VRAM + 512KB SPU RAM + BIOS    │
└─────────────────────────────────────────────────────────────┘
```

**Ciclo principal (simplificado):**
1. Frontend chama `emulator.run_frame()` ou `emulator.run_for(cycles)`.
2. Core executa CPU até VBlank / número de ciclos.
3. GPU produz framebuffer.
4. Core expõe ponteiro/offset do framebuffer via `wasm-bindgen`.
5. Frontend faz `texSubImage2D` + draw.
6. Áudio samples são enviados via callback.

---

## 4. Especificação de Agentes (Multi-Agent Setup para VS Code Copilot / Cursor / Aider)

Cada agente tem escopo estrito, contexto e responsabilidades. Use-os em ordem ou em paralelo conforme dependências.

### Agente 1: Arquiteto de Sistema (Architecture Agent)

**Nome sugerido:** `@architect`  
**Responsabilidades:**
- Definir a estrutura de crates (`psx-core`, `psx-wasm`, `psx-frontend`).
- Definir interfaces públicas do core (API que o frontend consome).
- Escolher entre interpreter puro vs. eventual dynarec (começar com interpreter).
- Definir memory map exato conforme PSX-SPX.
- Planejar suporte a multi-threading futuro (SharedArrayBuffer).
- Documentar decisões de arquitetura (ADRs).

**Entregáveis:**
- `docs/architecture.md`
- `Cargo.toml` workspace
- Diagrama de crates e módulos.

**Prompt base:**
```
Você é o Arquiteto de um emulador PSX em Rust→WASM. Baseie-se estritamente nas especificações nocash (psx-spx.consoledev.net). Priorize simplicidade, precisão e facilidade de manutenção. Nunca adicione features desnecessárias.
```

### Agente 2: UI/UX Agent

**Nome sugerido:** `@uiux`  
**Responsabilidades:**
- Interface mínima e responsiva (drag-and-drop de BIOS, ISO, Memory Card).
- Canvas com aspect ratio correto (4:3 nativo, com opções de stretch/CRT).
- Controles de teclado + Gamepad com remapeamento.
- Indicadores de FPS, status de BIOS carregada, save states.
- Acessibilidade básica e suporte a mobile (touch controls opcional).
- Tema dark minimalista.

**Entregáveis:**
- `frontend/src/` completo (HTML + TS + CSS).
- Design system simples (cores, tipografia).

**Prompt base:**
```
Você é especialista em UI/UX para emuladores web. Crie uma interface extremamente limpa, sem frameworks pesados (apenas Vanilla TS + CSS). Priorize drag-and-drop, feedback visual claro e zero distrações durante o jogo.
```

### Agente 3: Emulação Core – CPU + Bus + Interrupts (CPU Agent)

**Nome sugerido:** `@cpu`  
**Responsabilidades:**
- Implementar R3000A (todas as instruções MIPS I + quirks PSX).
- COP0 (exceptions, cache control).
- Bus de memória com waitstates e mirrors corretos.
- Interrupts e DMA basic.
- Testes unitários contra suites conhecidas (Amidog CPU tests, etc.).

**Referência obrigatória:** Seção "CPU Specifications" do PSX-SPX.

### Agente 4: GTE Agent

**Nome sugerido:** `@gte`  
**Responsabilidades:**
- Implementar Geometry Transformation Engine completo (todos os comandos).
- Precisão de fixed-point e quirks de divisão.

### Agente 5: GPU Agent (mais crítico)

**Nome sugerido:** `@gpu`  
**Responsabilidades:**
- GP0/GP1 command interpreter.
- VRAM 1024×512 (ou 1024×1024 mirrored).
- Rasterização de polygons, lines, rectangles (com affine texture, dither, etc.).
- Display area, drawing area, texture pages, CLUT.
- Status register e timing aproximado.
- Exposição do framebuffer para o frontend (ponteiro seguro).

**Prioridade de renderização:** Software rasterizer primeiro (mais preciso e simples de debugar). Depois aceleração via WebGL compute/shaders se necessário.

### Agente 6: SPU + Áudio Agent

**Nome sugerido:** `@spu`  
**Responsabilidades:**
- SPU completo (ADPCM, reverb, 24 voices).
- Geração de samples e entrega via Web Audio.

### Agente 7: CD-ROM + Disc Agent

**Nome sugerido:** `@cdrom`  
**Responsabilidades:**
- Emulação do controlador CD-ROM.
- Leitura de imagens BIN/CUE, ISO, e preferencialmente CHD no futuro.
- Seek times aproximados, subchannel, etc. (começar com high-level + timing razoável).

### Agente 8: Controllers + Memory Card Agent

**Nome sugerido:** `@sio`  
**Responsabilidades:**
- SIO0 (controllers + memory cards).
- Protocolo digital e analog (DualShock).
- Formato .mcd (128KB) e multi-card.
- Persistência via IndexedDB.

### Agente 9: BIOS + Kernel Agent

**Nome sugerido:** `@bios`  
**Responsabilidades:**
- Carregamento de BIOS real (SCPH-1001, 5501, 7001, etc.).
- Opcionalmente suporte a OpenBIOS (HLE) para homebrew.
- Nunca distribuir BIOS copyrighted.

### Agente 10: Testes Agent

**Nome sugerido:** `@tests`  
**Responsabilidades:**
- Testes unitários por componente (`cargo test`).
- Testes de integração (boot de BIOS, execução de demos homebrew).
- Suite de regressão com jogos conhecidos (lista de compatibilidade).
- Testes E2E com Playwright (carregar BIOS → carregar ISO → verificar framebuffer não preto).
- Integração com testes públicos da comunidade Emudev (Amidog, etc.).

### Agente 11: Build / CI / CD / Versionamento Agent

**Nome sugerido:** `@cicd`  
**Responsabilidades:**
- GitHub Actions:
  - Build WASM em todo PR.
  - Testes unitários + E2E.
  - Deploy automático para GitHub Pages / Cloudflare Pages.
  - Artefatos de release (wasm + frontend).
- Semantic Versioning.
- Conventional Commits.
- Changelog automático.
- Headers de Cross-Origin Isolation (COOP/COEP) para SharedArrayBuffer futuro.

**Pipeline sugerido:**
```yaml
on: [push, pull_request]
jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - run: cargo test --workspace
      - run: wasm-pack build --target web
      - run: npm ci && npm run test:e2e
  deploy:
    if: github.ref == 'refs/heads/main'
    ...
```

---

## 5. Roadmap de Implementação (Fases)

### Fase 0 – Setup (1 semana)
- Workspace Cargo + Vite.
- Skeleton WASM + frontend.
- CI básico.

### Fase 1 – CPU + Bus + BIOS Boot (3–5 semanas)
- R3000A interpreter funcional.
- BIOS carrega e executa até o shell (logo Sony + menu).

### Fase 2 – GPU Básico + Display (4–6 semanas)
- Comandos mais comuns (triangles, rectangles).
- Framebuffer visível.
- Primeiros demos homebrew rodando.

### Fase 3 – GTE + 3D (3–4 semanas)
- Jogos 3D simples começam a mostrar geometria.

### Fase 4 – CD-ROM + Controllers + Memory Card (4 semanas)
- Carregar jogos comerciais.
- Controles e saves funcionais.

### Fase 5 – SPU + Áudio + Polish (3–4 semanas)
- Som completo.
- Save states, fast-forward, etc.

### Fase 6 – Otimizações + Escalabilidade
- Dynarec (opcional).
- Multi-threading.
- WebGPU backend experimental.
- Compatibilidade mobile.

**Meta de compatibilidade inicial:** 60–70% dos jogos populares (Crash, Spyro, FF7, MGS, etc.) em velocidade full.

---

## 6. Recursos Obrigatórios de Referência

1. **PSX-SPX (nocash)** – https://psx-spx.consoledev.net/ (fonte da verdade)
2. **Original nocash:** https://problemkaputt.de/psx-spx.htm
3. **Emudev Discord** e subreddit r/EmuDev
4. Testes: Amidog PSX tests, PCSX-Redux test suite
5. Exemplos de código open-source (estudo apenas, não copiar):
   - https://github.com/annethereshewent/RSX
   - https://github.com/maxpoletaev/nupsx
   - https://github.com/ImLunaHey/emulators (packages/ps1)
   - DuckStation (referência de precisão moderna)

**Importante:** Não embutir BIOS ou ISOs. O usuário deve fornecer os próprios arquivos legalmente obtidos.

---

## 7. Prompt Completo Copiável para VS Code Copilot / Cursor (Multi-Agent)

Copie o bloco abaixo e cole como system prompt ou como instrução inicial do projeto:

```
# Projeto: Emulador PlayStation 1 (PSX) 100% WebAssembly + WebGL2

## Objetivo
Criar um emulador de PlayStation 1 completo, preciso e performático que rode inteiramente no navegador. Deve carregar BIOS originais, imagens de disco (BIN/CUE/ISO), Memory Cards (.mcd) e suportar controles (teclado + Gamepad).

## Stack obrigatória (não desviar)
- Core: Rust compilado para WebAssembly (wasm-pack + wasm-bindgen)
- Frontend: Vanilla TypeScript + Vite (zero frameworks)
- Renderização: WebGL2 puro (blit de framebuffer – a mais leve possível). NUNCA usar Three.js, Babylon.js ou qualquer engine 3D.
- Áudio: Web Audio API
- Persistência: IndexedDB + File System Access API
- Specs: baseadas estritamente em https://psx-spx.consoledev.net/ (nocash)

## Princípios
- Simplicidade e enxutez acima de tudo
- Renderização o mais leve possível
- Escalabilidade (preparado para dynarec e multi-threading futuro)
- Código limpo, testável, documentado
- Nunca distribuir BIOS ou jogos copyrighted

## Agentes especializados (use @nome quando necessário)

### @architect
Define a arquitetura de crates, memory map, interfaces públicas do WASM e decisões de design. Entrega docs/architecture.md e estrutura inicial.

### @uiux
Cria a interface mínima, limpa e responsiva (drag-and-drop de arquivos, canvas 4:3, controles, indicadores de status). Apenas Vanilla TS + CSS.

### @cpu
Implementa o processador R3000A + COP0 + bus de memória + interrupts básicos + DMA. Deve passar testes de CPU conhecidos.

### @gte
Implementa o Geometry Transformation Engine completo com precisão fixed-point.

### @gpu
Implementa o GPU (GP0/GP1, VRAM, rasterização de polígonos/linhas/retângulos, texture mapping affine, dithering, etc.). Expõe o framebuffer de forma eficiente para o frontend.

### @spu
Implementa o Sound Processing Unit e a entrega de samples para Web Audio.

### @cdrom
Implementa o controlador de CD-ROM e o carregamento de imagens de disco.

### @sio
Implementa controllers (digital + DualShock) e Memory Cards com persistência.

### @bios
Gerencia o carregamento de BIOS reais e suporte opcional a OpenBIOS (HLE).

### @tests
Cria e mantém testes unitários, de integração e E2E (Playwright). Mantém lista de compatibilidade.

### @cicd
Configura GitHub Actions, Semantic Versioning, Conventional Commits, deploy automático e headers de Cross-Origin Isolation.

## Ordem de implementação recomendada
1. Setup do monorepo + CI
2. CPU + Bus + boot de BIOS até o logo/shell
3. GPU básico + display de framebuffer
4. GTE
5. CD-ROM + Controllers + Memory Card
6. SPU
7. Polish, save states, otimizações

## Regras para todos os agentes
- Sempre citar a seção do PSX-SPX quando implementar um componente.
- Preferir clareza a micro-otimizações prematuras.
- Todo código Rust deve ter testes unitários.
- Frontend deve funcionar offline após o primeiro load (PWA opcional).
- Performance alvo: full speed em hardware de 2022+.

Comece pelo @architect criando a estrutura inicial do projeto.
```

---

## 8. Estrutura de Diretórios Sugerida

```
psx-web-emulator/
├── Cargo.toml                 # workspace
├── crates/
│   ├── psx-core/              # lógica pura (no_std friendly)
│   │   ├── src/
│   │   │   ├── cpu/
│   │   │   ├── gte/
│   │   │   ├── gpu/
│   │   │   ├── spu/
│   │   │   ├── cdrom/
│   │   │   ├── bus.rs
│   │   │   └── lib.rs
│   │   └── tests/
│   └── psx-wasm/              # bindings wasm-bindgen
│       └── src/lib.rs
├── frontend/
│   ├── index.html
│   ├── src/
│   │   ├── main.ts
│   │   ├── renderer.ts        # WebGL2 blit
│   │   ├── audio.ts
│   │   ├── input.ts
│   │   └── ui.ts
│   ├── package.json
│   └── vite.config.ts
├── docs/
│   ├── architecture.md
│   └── compatibility.md
├── .github/workflows/
│   └── ci.yml
└── README.md
```

---

## 9. Considerações Legais e Éticas

- O emulador **não** inclui BIOS nem jogos.
- O usuário deve fornecer arquivos que possui legalmente (dump da própria console).
- Documentação clara sobre isso no README e na UI.
- Licença recomendada: GPL-2.0-or-later ou MIT (escolher conforme uso de código de referência).

---

## 10. Conclusão

A abordagem **WebAssembly bare metal com Rust + WebGL2** é a única que entrega:
- Máxima simplicidade de manutenção a longo prazo
- Código enxuto
- Escalabilidade real
- Renderização mais leve possível
- Compatibilidade real com software original do PSX

Three.js deve ser descartado completamente para este objetivo.

Este plano está pronto para ser usado como base de um projeto multi-agente no VS Code Copilot / Cursor. Basta copiar o prompt da seção 7 e começar pelo agente `@architect`.
