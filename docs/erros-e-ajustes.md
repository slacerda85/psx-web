# Erros abertos e pontos a ajustar

Estado detalhado do que está errado ou incompleto no emulador, com a evidência
que sustenta cada item e o que fazer a seguir. Complementa
[pendencias-hardware.md](pendencias-hardware.md), que é o placar resumido
contra o console; aqui está o diagnóstico.

Última revisão: 27/08/2026, commit `d7b9cff`.

Portões verdes: `cargo fmt` limpo, `cargo clippy -D warnings` limpo, 258 testes
passando, BIOS renderizando 99,6% dos pixels de referência, 6 de 21 testes do
[ps1-tests](https://github.com/JaCzekanski/ps1-tests) batendo exatamente com o
hardware real.

**Onde os jogos chegam hoje:** os três discos de teste bootam e executam código
próprio. O Gran Turismo decodifica e **exibe** o vídeo de abertura; os outros
dois montam o display e param antes de desenhar.

---

## 1. Ferramentas de diagnóstico

Quatro exemplos em `crates/psx-core/examples/` existem só para investigar. Vale
conhecê-los antes de atacar qualquer item — cada bug abaixo foi isolado com um
deles, e a ordem em que foram usados é quase sempre a mesma: `hotspots` para
achar onde a CPU parou, `iotrace` para ver o que ela pede ao hardware,
`hwtest` para confirmar contra o console.

| Ferramenta | Para que serve |
| --- | --- |
| `hwtest` | Roda os `.exe` do ps1-tests e compara com o `psx.log` capturado do PlayStation real. Aceita `--disc`, `--only`, `--verbose`. |
| `hotspots` | Histograma de PC com desassemblador, `--dump <end>:<n>` para ler a RAM, `--watch` e as últimas linhas da TTY do kernel. |
| `iotrace` | Guarda os últimos N acessos ao bloco de I/O (endereço, valor, largura, PC) e ranqueia quem mais repete. Encontra laços de espera. |
| `screenshot` | Roda por N frames e grava BMP do framebuffer ou da VRAM. Aceita `--press` para injetar botões. |

Duas leituras valem mais que o resto:

- **A TTY do kernel.** O `hotspots` imprime as últimas linhas. Um
  `VSync: timeout` ou a ausência de `Execute !` diz em uma linha o que o
  histograma leva meia hora para sugerir.
- **`setores` contra `recolhidos`** no `debug_state` do CD-ROM. Entregue e
  recolhido são coisas diferentes, e a distância entre os dois separa "o drive
  não entrega" de "o jogo recusa o que recebeu".

```sh
cargo run --release -p psx-core --example hotspots -- \
    --bios bios/SCPH1001.BIN --disc games/xenogears/xenogears-disk-1.cue \
    --frames 4000 --skip 3900
```

O `ps1-tests` não vive no repositório. Baixar os binários já compilados:

```sh
curl -sL -o tests.zip \
  https://github.com/JaCzekanski/ps1-tests/releases/download/build-158/tests.zip
```

---

## 2. Onde os jogos param hoje

Três discos de teste, três resultados diferentes.

### Gran Turismo — toca o FMV de abertura

Boota, carrega `SCUS_941.94`, monta o display de 24 bits e **decodifica e
exibe o vídeo de abertura**: 80% dos pixels desenhados, VRAM 73% preenchida,
créditos da Polyphony Digital legíveis na tela. Recolhe 6779 de 6779 setores
entregues — não desperdiça um.

É o primeiro jogo a produzir imagem. Serve de referência: o caminho
CD → XA → MDEC → GPU funciona ponta a ponta.

### Xenogears — monta o FMV e para de consumir

Boota, carrega `SLUS_006.64`, monta o display de 24 bits em 320×240 a partir
de `vram_y=240` e começa a ler o STR de abertura com `ReadS` em modo `0xC8`.
Recolhe 365 setores e para, enquanto o drive continua entregando.

A CPU fica no laço de espera de VBlank da `VSync()`. O contador de VBlank
**está** avançando — verificado no watchpoint, escrito pelo callback do jogo
em `0x8004BFA4` a cada quadro. O MDEC nunca é tocado: zero acessos a
`0x1F801820`/`0x1F801824` numa janela de 120 mil acessos de I/O.

O jogo trata as interrupções do CD o tempo todo (1416 acknowledges na mesma
janela) e lê as respostas, mas não pede os dados. Ou seja: o callback roda e
recusa o setor.

A região que ele lê tem um canal só (arquivo 1, canal 1), 1746 setores de
vídeo (submodo `0x42`) e 249 de áudio — então não é filtragem de canal.

### Grandstream Saga — não liga o display

Boota, carrega `SLUS_005.97` e um segundo overlay, chama `ResetGraph` duas
vezes e fica lendo. `display disabled=true` do começo ao fim, zero pixels.
Recolhe 401 setores e para, no mesmo padrão do Xenogears.

## 3. Bugs isolados contra o hardware

### `cpu/code-in-io` — buscar instrução da região do MDEC prende o emulador

A scratchpad já dá bus error corretamente (`is_executable` em
[memory/mod.rs](../crates/psx-core/src/memory/mod.rs)). Quando o PC aponta para
a região do MDEC, o emulador entra num laço em vez de levantar a exceção.

⚠️ **Não torne o bloco de I/O inteiro não-executável.** O comentário do
`is_executable` registra que o próprio teste confirma que SPU e DMA **são**
buscáveis no console. O caso real é estreito: só o MDEC. Comece lendo o
`psx.log` do teste, não presumindo a regra.

### `cpu/io-access-bitwidth` — leituras estreitas sem tratamento

`load_io16` e `load_io8` em [bus.rs](../crates/psx-core/src/bus.rs) não tratam
`0x810`, `0x814`, `0x820`, `0x824` nem o bloco de DMA `0x080..0x0FF`; tudo cai
em `unhandled_read` e devolve barramento flutuante. A lacuna é maior que os
"dois campos" que se supunha.

Um `load_io_wide` simétrico ao `store_io_wide` que já existe resolve a classe
inteira: o periférico sempre vê 32 bits, o barramento recorta o lane.

### `cdrom/getloc` — 4 falhas

Bits de status durante seek e leitura, e as respostas de `GetlocL`/`GetlocP`.
O `enum Drive` só tem `Idle` e `Reading` — não existe estado `Seeking`, e os
bits `SEEKING`/`READING` são ligados à mão em cada comando. O mutex
Play/Seek/Read do PSX-SPX não está modelado.

⚠️ **Armadilha já pisada:** condicionar o `GetlocP` a um flag `position_valid`
piorou o resultado líquido — trocou "falha esperada que passava" por "sucesso
esperado que falhava". Foi revertido. Meça pelo total de casos, não pelo caso
que está olhando.

---

## 4. Subsistemas incompletos

### SPU — toca, sem reverb

As 24 vozes com SPU-ADPCM, envelope ADSR, tom com interpolação linear e
modulação, ruído, `ENDX` e a IRQ de endereço estão implementados, além da
entrada de áudio do CD reamostrada e misturada à saída.

Falta **reverb** e a janela gaussiana de quatro pontos na interpolação. O
primeiro é um efeito, o segundo é timbre; nenhum muda o comportamento
observável de um jogo.

No `spu/memory-transfer` passam `testDtcRegister`, `testManualWriteToSpuRam`,
`testDMAReadFromSpuRam` e `testDMAWriteToSpuRam`. Restam os casos de timing,
que dependem do DMA deixar de ser atômico, e dois de SyncMode 0 que **não
existem no log capturado do console** — o binário do release é mais novo que a
captura. Sem referência, não dá para persegui-los; um teste unitário confirma
que o caminho funciona nos dois sentidos.

### MDEC — decodifica certo, arredonda diferente

O decodificador e o DMA de saída funcionam. Comparado ao console em 8 bpp, a
maioria dos bytes bate exatamente e alguns divergem em 1 ou 2 unidades — é
arredondamento dentro do IDCT, não erro estrutural. Não é bloqueante.

O Gran Turismo o exercita de ponta a ponta no vídeo de abertura; os outros dois
não chegam a tocá-lo.

---

## 5. Timing de barramento — o que sobrou

O tempo de acesso à memória **foi implementado** (seção 7) e o
`cpu/access-time` agora fica dentro de um ciclo do console em todas as regiões.
Ele continua marcado como "diferente" no `hwtest` porque o log de referência
traz médias com jitter — o console mede 5,21, 5,3 e 5,14 ciclos para o mesmo
acesso à RAM, e igualdade textual é impossível por construção.

O que continua faltando é o **custo em ciclos do DMA e do desenho da GPU**:
nossa transferência é atômica e acontece inteira dentro da escrita que a
dispara, sem cobrar um ciclo da CPU.

| Teste | O que mede |
| --- | --- |
| `dma/chopping` | Ciclos que o DMA rouba da CPU por bloco — reportamos 0 |
| `gpu/bandwidth` | Vazão de transferências para a VRAM |
| `cdrom/timing` | Latência de resposta dos comandos do drive |
| `timers` / `timer-dump` | Os contadores batem; falha o custo do DMA aparecer como zero |
| `dma/chain-looping` | O comportamento observável já bate; sobra o custo em ciclos |

Resolver isso é um scheduler que intercale DMA e CPU. Continua fora do MVP —
mas com uma ressalva que o tempo de acesso à memória já provou uma vez: uma
lacuna de timing pode ser a diferença entre "roda" e "não roda", não só entre
"preciso" e "aproximado".

---

## 6. Fora do escopo do MVP

- `cdrom/disc-swap` — exige um humano abrir a tampa.
- Memory card.
- CD-DA (faixas de áudio tocadas pelo drive).
- Bit-exatidão do MDEC (seção 4).
- Scheduler de ciclos de DMA/GPU (seção 5).

---

## 7. O que esta rodada corrigiu

Cada um destes foi medido antes e depois, não presumido.

### Tempo de acesso à memória — o que impedia qualquer jogo de rodar

Todo load e store custava um ciclo. No console a RAM cobra ~5, o BIOS de 8 a
25 conforme a largura, a SPU quase 40 (`cpu/access-time`, medido em hardware).

A consequência não era imprecisão, era travamento total: a `VSync()` da
biblioteca da Sony espera o VBlank com um prazo contado em **iterações de um
laço**, não em tempo. O laço tem 9 instruções e 3 acessos à RAM; a 1 ciclo por
instrução o prazo esgotava na metade do frame. O kernel imprimia
`VSync: timeout` em todo quadro, nos dois jogos, e nenhum passava do boot.

Com os tempos certos, os dois chegam ao `Execute !`.

### Detector de borda do DICR

A IRQ de DMA é disparada pela transição de 0 para 1 do flag mestre do `DICR`,
e o nível anterior só era atualizado ao rodar uma transferência. Quando o
software reconhecia o flag, o detector continuava achando que o nível era 1: a
transferência seguinte levantava o flag de novo, a transição nunca aparecia e
a interrupção nunca chegava.

No Xenogears era a transferência de samples para a SPU RAM. A primeira
passava, a segunda travava o carregamento para sempre. Corrigido, o jogo sai
de 299 setores lidos para quase 7000 e monta o display do FMV.

### Setores de áudio XA

Com o bit 6 do `Setmode`, o drive manda os setores de áudio ao decodificador
ADPCM em vez de entregá-los como dados. Entregávamos todos. No Grandstream,
1336 dos 3470 setores entregues eram áudio (submodo `0x64`).

A entrega ao CPU também segue agora as **duas tentativas** do hardware
(PSX-SPX, "Data/ADPCM Sector Filtering/Delivery"): a primeira ignora arquivo e
canal, a segunda confere.

---

## 8. Ordem sugerida de ataque

1. **Por que Xenogears e Grandstream param de recolher setores.** O Gran
   Turismo faz o mesmo caminho e funciona, então há uma referência viva para
   comparar: rode os dois lado a lado e veja onde o callback de CD diverge.
2. **`cpu/io-access-bitwidth`** — um `load_io_wide` fecha a classe inteira.
3. **`cpu/code-in-io`** — pequeno, mas leia o `psx.log` antes de decidir a regra.
4. **`cdrom/getloc`** — precisa do estado `Seeking` e do mutex de status.
5. **Reverb da SPU**, quando o resto estiver soando.

## Apêndice — classes de erro já corrigidas

Cada uma apareceu mais de uma vez. Ficam registradas como padrão a vigiar.

| Classe | Onde apareceu |
| --- | --- |
| Custo de acesso à memória tratado como uniforme | CPU: um ciclo para tudo, quando a RAM cobra cinco |
| Detector de borda sem acompanhar quem limpa o nível | DMA: `previous_master_flag` e o ack do `DICR` |
| Registrador de 16 bits tratado como 32 | GTE: truncar **na escrita**, não na leitura |
| Saturação calculada a partir de valor já truncado | GTE: `SX2`/`SY2`/`OTZ` vinham do MAC0 truncado |
| Overflow checado em bloco em vez de por estágio | GTE: cada produto tem sua própria flag |
| Escrever direto no acumulador engolindo flags | GTE: `apply_vertex_color` |
| Coprocessador habilitado deve virar no-op, não exceção | CPU: COP1/COP3 |
| Checagem de `SR.CU` faltando | CPU: `LWC2`/`SWC2` |
| Bit de status sem checar a permissão que o governa | GPU: bit 15 do `GPUSTAT` e o `GP1(0x09)` |
| Máscara de escrita ignorada | GPU: `GP0(0xE6)` na transferência CPU→VRAM |
| Significado do bit invertido | GPU: bit 27 é "há dado esperando", não "GPU ociosa" |
| Canal cabeado tratado como configurável | DMA: canal 6 (OTC) |
| Sinal simultâneo que na verdade é atrasado | SIO0: `/ACK` chega ~338 ciclos depois |
| Avanço em bloco onde o hardware avança por instrução | Timers, e depois o CD-ROM |
| Divisor inteiro onde a razão é fracionária | Timers: dot clock é 11/7 |
| Escrita estreita perdendo os bytes altos | Bus: o barramento não tem byte-enables |
| Entrega em etapa única onde o hardware tem duas | CD-ROM: filtragem de arquivo/canal só na segunda |

### Sobre offsets negativos ao ler desassemblagem

Três vezes nesta sessão eu li o endereço errado de um `lhu`/`lw` por esquecer
que o imediato é sinalizado. `lui r2,0x8006` seguido de `lhu r2,0x957C(r2)`
lê de `0x8005957C`, não de `0x8006957C` — `0x957C` é negativo. Errar isso faz
um watchpoint parecer que "ninguém escreve nessa variável".

---

## Restrição permanente

BIOS e imagens de jogo são material protegido por direitos autorais e **nunca**
podem ser commitados ou distribuídos. O `.gitignore` bloqueia `/bios/` e
`/games/` por diretório, não por extensão. Auditar os objetos do git antes de
cada push.
