# Compatibilidade

## Estado

**Nenhum jogo comercial roda ainda.** O controlador de CD-ROM responde à
máquina de comandos, mas não carrega imagens BIN/CUE, ISO ou CHD — sem isso
não há de onde ler um jogo. Esse é o trabalho do agente `@cdrom` e o gargalo
da Fase 4 do [plano](plano-emulador-psx-web.md).

O que dá para exercitar hoje:

| Alvo | Situação |
| --- | --- |
| BIOS até o shell | `runUntilShell` existe; depende da BIOS que o usuário fornecer |
| Homebrew `PS-X EXE` | Carregável via `loadExe`, sem precisar de disco |
| Jogos comerciais | Bloqueado pelo carregamento de imagem de disco |

## Como testar

1. Carregue a sua BIOS (512 KB, extraída do seu próprio console).
2. Arraste um `.exe` de homebrew para a janela.
3. Abra o painel **Diagnóstico** e observe os contadores.

Os contadores são o instrumento principal de triagem. Se algo não roda, eles
dizem qual subsistema faltou antes de você precisar abrir um debugger:

| Contador | O que significa |
| --- | --- |
| `gteUnimplemented` | Um comando COP2 caiu no caso padrão |
| `gpuUnhandled` | Um comando GP0/GP1 não foi reconhecido |
| `cdromUnimplemented` | Um comando de CD-ROM não foi reconhecido |
| `busUnhandledReads` / `busUnhandledWrites` | Acesso a um endereço sem periférico mapeado |

Contador subindo depressa quase sempre aponta o subsistema culpado. Contadores
todos zerados com tela preta indicam problema de CPU, timing ou display — não
de funcionalidade faltando.

## Registrando um resultado

Ao testar um título, registre nesta tabela. Sem BIOS e sem jogo anexados:
apenas o resultado observado.

| Jogo | Região | Resultado | Contadores relevantes | Observações |
| --- | --- | --- | --- | --- |
| _(nenhum ainda)_ | | | | |

Escala de resultado:

- **Perfeito** — roda do início ao fim, áudio e vídeo corretos.
- **Jogável** — completável, com falhas gráficas ou de áudio menores.
- **Menu** — chega ao menu ou à intro e trava.
- **Boot** — a BIOS reconhece o disco mas o jogo não inicia.
- **Nada** — não passa da tela preta.

## Meta

A meta declarada no plano para a primeira versão utilizável é **60–70% dos
títulos populares** (Crash, Spyro, FF7, MGS) em velocidade cheia. Chegar lá
depende, em ordem: carregamento de disco (`@cdrom`), mixagem do SPU (`@spu`)
e memory cards (`@sio`).
