# Regras comuns a todos os agentes do projeto psx-web-emulator

> Este arquivo NÃO é um agente. É o contrato compartilhado referenciado por todos
> os agentes em `.claude/agents/*.md`. Leia-o antes de qualquer implementação.

## Stack obrigatória (não desviar)
- Core: **Rust** (edition 2021) compilado para **WebAssembly** (`wasm-pack` + `wasm-bindgen`).
- Frontend: **Vanilla TypeScript + Vite**. Zero frameworks (sem React/Vue/Svelte).
- Renderização: **WebGL2 puro** — blit de framebuffer via `texSubImage2D`.
  **NUNCA** usar Three.js, Babylon.js ou qualquer engine 3D.
- Áudio: Web Audio API (AudioWorklet preferencial).
- Persistência: IndexedDB + File System Access API.
- Specs: <https://psx-spx.consoledev.net/> é a **fonte da verdade**.

## Princípios
1. Simplicidade e enxutez acima de tudo.
2. Precisão antes de performance; clareza antes de micro-otimização.
3. Escalabilidade: preparado para dynarec e multi-threading (SharedArrayBuffer) no futuro.
4. Todo código Rust novo vem com testes unitários (`cargo test`).
5. **Nunca** embutir, baixar ou distribuir BIOS ou jogos com copyright.

## Convenções de código
- Rust: `cargo fmt` + `cargo clippy -- -D warnings` limpos antes de concluir.
- Nomes de registradores e campos seguem a nomenclatura do PSX-SPX
  (`GPUSTAT`, `I_STAT`, `I_MASK`, `DICR`, `MADR`, `BCR`, `CHCR`, ...).
- Todo módulo de hardware documenta no topo a seção do PSX-SPX que implementa:
  `//! Referência: PSX-SPX — "GPU Render Polygon Commands"`.
- Endereços sempre em hex com underscore: `0x1F80_1810`.
- TypeScript: `strict: true`, sem `any` implícito.

## Fluxo de trabalho
- Antes de editar, leia `docs/architecture.md` e os módulos vizinhos.
- Rode `cargo test --workspace` ao terminar. Reporte falhas com a saída real.
- Commits seguem **Conventional Commits** (`feat(gpu): ...`, `fix(cpu): ...`).
- Não comite nem faça push sem o usuário pedir.

## Limites de escopo
Cada agente altera apenas os arquivos do seu domínio (listados na sua definição).
Se precisar mudar código de outro domínio, descreva a mudança necessária no
relatório final em vez de aplicá-la silenciosamente.
