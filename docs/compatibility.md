# Compatibilidade

## Estado

O CD-ROM lê imagens (ISO, BIN cru e BIN/CUE) e entrega setores por FIFO e por
DMA. O Xenogears carrega do disco e executa o próprio código, com **zero**
comandos de CD-ROM sem implementação.

O GTE está implementado: os 22 comandos, a aritmética de 44 bits, as flags de
saturação e a divisão UNR.

**O Xenogears carrega, executa 299 setores de código próprio e depois trava**
em `0x8003BE08`, girando num laço que espera o bit 4 de `0x8006957C` mudar.
Todos os contadores de diagnóstico ficam em zero — não é funcionalidade
faltando, é comportamento errado em algum lugar que os contadores não veem.

O histórico de comandos do drive no momento da trava é
`[Pause, Setloc, ReadN, Pause, Setloc, ReadN, …]` repetindo, com o contador de
setores entregues parado: o jogo pede a leitura, não recebe o que espera e
tenta de novo, para sempre.

| Alvo | Situação |
| --- | --- |
| BIOS até o shell | Funciona: SCPH1001 chega ao menu principal |
| Homebrew `PS-X EXE` | Carregável via `loadExe`, sem precisar de disco |
| Carregar jogo do disco | Funciona: 299 setores lidos e executados |
| Xenogears além da abertura | Trava no laço de releitura descrito acima |

Achar isso pede comparar a execução instrução a instrução contra um emulador
de referência a partir do ponto em que o jogo assume o controle. Os exemplos
`hotspots` e `screenshot --vram` existem justamente para essa investigação.

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
