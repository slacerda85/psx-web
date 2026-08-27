# psx-web

Emulador de PlayStation 1 que roda inteiramente no navegador. O núcleo de
emulação é escrito em Rust e compilado para WebAssembly; o frontend é
TypeScript sem framework, com WebGL2 para vídeo e `AudioWorklet` para som.

> **Este projeto não distribui BIOS nem jogos.** Você precisa fornecer a BIOS
> extraída do seu próprio console. Nenhum arquivo sai do seu navegador — a
> BIOS fica no IndexedDB local e não é enviada a servidor nenhum.

## Estado atual

| Subsistema | Situação |
| --- | --- |
| CPU R3000A + COP0 | Interpretador MIPS I completo, exceções e delay slots |
| Bus / mirrors / DMA / Timers / IRQ | Implementados |
| GTE (COP2) | 22 comandos, aritmética de 44 bits, flags de saturação e a divisão UNR |
| GPU | VRAM 1024×512, rasterizador por software, GP0/GP1, GPUSTAT |
| SPU | Registradores, SPU RAM, DMA e geração de amostras; **mixagem das 24 vozes pendente** |
| CD-ROM | Leitura de ISO, BIN cru e BIN/CUE; seek, leitura contínua 1x/2x, TOC e DMA |
| SIO0 | Controller digital completo; **DualShock e memory card pendentes** |
| Frontend | Vídeo, áudio, entrada, drag-and-drop, remapeamento e persistência |

Cobertura atual: **230 testes** no core (227 unitários + 3 de integração contra
uma BIOS real) e **10 testes E2E** no frontend.

O painel *Diagnóstico* da interface mostra contadores de funcionalidade não
implementada em tempo real — se um jogo não roda, os números ali dizem qual
subsistema faltou.

## Requisitos

- Rust estável com o target `wasm32-unknown-unknown`
- Node 22 ou superior
- Um navegador com WebGL2 e `AudioWorklet` (Chrome, Edge, Firefox, Safari 16+)

```sh
rustup target add wasm32-unknown-unknown
```

O `wasm-pack` vem como dependência de desenvolvimento do frontend — não é
preciso instalá-lo globalmente.

## Rodando

```sh
cd frontend
npm install
npm run dev
```

Abra `http://localhost:5173` e arraste a sua BIOS para a janela (ou use
*Escolher BIOS…*). Com a BIOS no lugar:

- **Jogo em ISO ou BIN de faixa única:** arraste o arquivo, ou use *Inserir disco…*.
- **Jogo em CUE+BIN:** arraste os **dois arquivos juntos**. A folha sozinha não
  diz onde estão os dados, e o nome que ela declara quase nunca bate com o
  binário baixado.
- **Homebrew:** *Carregar .exe*, sem precisar de disco.

Uma imagem de jogo passa de 700 MB e é lida inteira para a memória. Funciona
em desktop, mas é pesado — a leitura sob demanda ainda não existe.

## Comandos

| Comando | O que faz |
| --- | --- |
| `cargo test --workspace` | Testes do núcleo de emulação |
| `cargo clippy --workspace --all-targets` | Lint (o CI trata avisos como erro) |
| `cargo fmt --all` | Formatação |
| `npm run dev` | Compila o WASM e sobe o dev server |
| `npm run build` | Build de produção (wasm-pack + `tsc` + Vite) |
| `npm run test:e2e` | Testes E2E do frontend com Playwright |

Os comandos `npm` rodam a partir de `frontend/`.

## Controles padrão

| Tecla | Botão | Tecla | Botão |
| --- | --- | --- | --- |
| Setas | D-Pad | <kbd>Enter</kbd> | Start |
| <kbd>W</kbd> | Triângulo | <kbd>Shift dir.</kbd> | Select |
| <kbd>S</kbd> | X | <kbd>Q</kbd> / <kbd>E</kbd> | L1 / R1 |
| <kbd>A</kbd> | Quadrado | <kbd>Z</kbd> / <kbd>C</kbd> | L2 / R2 |
| <kbd>D</kbd> | Círculo | <kbd>F</kbd> / <kbd>G</kbd> | L3 / R3 |

Gamepads no perfil *Standard* são detectados automaticamente. O mapeamento do
teclado pode ser trocado em *Controles…* e fica salvo no navegador.

## Estrutura

```
crates/psx-core/     Lógica de emulação pura, sem dependências e sem navegador
crates/psx-wasm/     Bindings wasm-bindgen (camada fina, sem lógica)
frontend/            Vite + TypeScript: vídeo, áudio, entrada e UI
frontend/e2e/        Testes Playwright
.claude/agents/      Definições dos 11 agentes especializados do projeto
docs/                Plano, arquitetura e notas de deploy
```

`psx-core` não conhece o navegador nem o sistema de arquivos: quem entrega
bytes é o embedder. É isso que permite testar a emulação inteira fora do
navegador, com `cargo test`.

Ver [docs/architecture.md](docs/architecture.md) para o desenho do sistema e
[docs/plano-emulador-psx-web.md](docs/plano-emulador-psx-web.md) para o plano
original com o roadmap por fases.

## Contribuindo

Commits seguem [Conventional Commits](https://www.conventionalcommits.org/)
(`feat:`, `fix:`, `docs:`, …) — o CI verifica as mensagens em cada PR e o
changelog de release é gerado a partir delas.

Cada subsistema tem um agente responsável descrito em
[.claude/agents/](.claude/agents/), com o escopo e as referências de hardware
que ele deve seguir. A fonte da verdade para comportamento de hardware é o
[PSX-SPX](https://psx-spx.consoledev.net/).

## Legal

Emuladores são legais. BIOS e jogos são obras protegidas: use apenas cópias
que você mesmo extraiu de hardware e mídia que possui. Este repositório não
contém, não distribui e não ajuda a obter nenhum dos dois.

Código sob licença [MIT](LICENSE).
