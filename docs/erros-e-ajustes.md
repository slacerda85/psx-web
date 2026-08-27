# Erros abertos e pontos a ajustar

Estado detalhado do que está errado ou incompleto no emulador, com a evidência
que sustenta cada item e o que fazer a seguir. Complementa
[pendencias-hardware.md](pendencias-hardware.md), que é o placar resumido
contra o console; aqui está o diagnóstico.

Última revisão: 27/08/2026, commit `83ad0a5`.

Portões que estão verdes hoje: `cargo fmt` limpo, `cargo clippy -D warnings`
limpo, 236 testes passando, BIOS renderizando 99,6% dos pixels de referência,
6 de 21 testes do [ps1-tests](https://github.com/JaCzekanski/ps1-tests) batendo
exatamente com o hardware real.

---

## 1. Ferramentas de diagnóstico

Quatro exemplos em `crates/psx-core/examples/` existem só para investigar. Vale
conhecê-los antes de atacar qualquer item abaixo — cada bug daqui foi isolado
com um deles.

| Ferramenta | Para que serve |
| --- | --- |
| `hwtest` | Roda os `.exe` do ps1-tests e compara a saída com o `psx.log` capturado do PlayStation real. Aceita `--disc`, `--only`, `--verbose`. |
| `iotrace` | Guarda os últimos N acessos ao bloco de I/O (endereço, valor, largura, PC) e imprime um ranking de quem mais repete. Encontra laços de espera. |
| `hotspots` | Histograma de PC com mini-desassemblador e `--watch`. Mostra onde o código está preso. |
| `screenshot` | Roda por N frames e grava BMP do framebuffer ou da VRAM. Aceita `--press` para injetar botões. |

```sh
cargo run --release -p psx-core --example iotrace -- \
    --bios bios/SCPH1001.BIN --disc games/xenogears/xenogears-disk-1.cue \
    --frames 1500 --tail 60
```

---

## 2. Bug aberto nº 1 — a trava do Xenogears

**O sintoma.** O jogo carrega, executa 299 setores do próprio código e entra num
laço infinito `Pause → Setloc → ReadN`. Nenhum contador de diagnóstico dispara:
não há exceção, não há acesso inválido, não há fila cheia. Não é funcionalidade
faltando — é um valor errado sendo devolvido em algum registrador.

### O que o rastro de I/O mostrou

Janela de 400 mil acessos, 1500 frames, jogo já travado:

| Registrador | Leituras | Escritas | Endereço |
| --- | --- | --- | --- |
| `I_STAT` | 145.008 | 8.290 | `0x1F801070` |
| `TIMER` | 72.305 | 0 | `0x1F801100..12F` |
| `GPUSTAT` | 72.304 | 0 | `0x1F801814` |
| **`DMA3_CHCR`** | **36.359** | **221** | `0x1F8010B8` |
| `I_MASK` | 23.136 | 0 | `0x1F801074` |
| `JOY_CTRL` | 5.310 | 11.505 | `0x1F80104A` |
| `CDROM_STAT` | 618 | 880 | `0x1F801800` |

**A pista forte é o `DMA3_CHCR`: 164 leituras por escrita.** O jogo arma o DMA
do CD-ROM e fica lendo o registrador de controle esperando algo. Nenhum outro
canal tem essa razão.

### O que já foi medido e descartado

Cada um destes foi instrumentado e verificado, não presumido:

- **Não é o MDEC.** Implementá-lo por inteiro não mudou nada no ponto da trava.
- **Não é o GTE, a GPU nem o SIO0** — os três batem com o console nos testes.
- **Não é o mapeamento de LBA.** O header BCD do setor entregue confere com o
  MSF que o jogo pediu.
- **Não é o fim do disco** nem o gate de acknowledge do drive: ambos
  instrumentados, nenhum é atingido durante a trava.
- **Não é o caminho de VBlank.** No rastro, o handler do jogo em `0x8004BA34`
  lê `I_STAT -> 0x0001`, escreve `0xFFFFFFFE` e a leitura seguinte devolve
  `0x0000`, exatamente como deveria.
- **Não é "o DMA nunca roda".** No mesmo período: canal da GPU 598 vezes,
  CD-ROM 444, OTC 326, SPU 15.

### A contradição sem explicação

Medindo do `ReadN` até o `Pause`, a espera do jogo acompanha a latência que
*nós* configuramos, mais ~331 ciclos, seja qual for o valor que colocarmos. É
como se o jogo reagisse ao nosso timing em vez de a um prazo fixo dele. Ao
mesmo tempo, o contador de `advance_read`
([cdrom/mod.rs:296](../crates/psx-core/src/cdrom/mod.rs#L296)) **não expira**
durante a trava, embora o drive esteja em `Reading` e o jogo espere mais ciclos
do que o período de um setor.

Essas duas observações não fecham entre si. Uma das duas medições está lendo o
estado errado, ou há um caminho que reinicia o contador que ainda não foi
mapeado.

### Próximo passo concreto

Antes de qualquer coisa cara: **instrumentar o laço do `DMA3_CHCR`**. Registrar,
para cada iteração, o PC que lê, o valor devolvido e o estado do CD-ROM naquele
instante (fase do drive, `sector_available`, fila de IRQ pendente). Duas
perguntas a responder:

1. O bit 24 (busy) está deixando de limpar depois da transferência?
2. Ou o valor está correto e o jogo espera um IRQ que não chega — caso em que a
   causa está no CD-ROM, não no DMA?

O rastro atual já dá o PC; falta cruzar com o estado do drive no mesmo ciclo.

---

## 3. Bugs isolados contra o hardware

Três testes do ps1-tests falham com causa localizada. Cada um é pequeno e
independente.

### `cpu/code-in-io` — buscar instrução da região do MDEC prende o emulador

A scratchpad já dá bus error corretamente (`is_executable` em
[memory/mod.rs](../crates/psx-core/src/memory/mod.rs)). O que resta: quando o PC
aponta para a região do MDEC, o emulador entra num laço em vez de levantar a
exceção. Provável causa: a busca de instrução em I/O não passa pela mesma
verificação que a leitura de dados, então devolve lixo que se desassembla como
um salto para si mesmo.

**Ajuste:** estender a checagem de executabilidade a todo o bloco de I/O, não
só à scratchpad, e confirmar contra o `psx.log` qual exceção o console levanta.

### `cpu/io-access-bitwidth` — dois campos restantes

O modelo de escrita estreita já está certo (a palavra inteira do registrador da
CPU chega ao periférico; a largura do periférico decide o resto — ver
`store_io_wide` em [bus.rs:436](../crates/psx-core/src/bus.rs#L436)). Sobram
dois pontos:

- O **campo de resolução no `GPUSTAT`** não responde como o console a leituras
  de 8 e 16 bits.
- A região de **Expansion 3** não está mapeada com o comportamento certo.

### `cdrom/getloc` — 4 falhas

Bits de status durante seek e leitura, e as respostas de `GetlocL`/`GetlocP`.

⚠️ **Armadilha já pisada:** uma tentativa de condicionar o `GetlocP` a um flag
`position_valid` piorou o resultado líquido — trocou "falha esperada que
passava" por "sucesso esperado que falhava". Foi revertida. Qualquer correção
aqui precisa ser medida pelo total de casos, não pelo caso que se está olhando.

---

## 4. Lacunas de timing — uma causa, seis testes

Nosso DMA é **atômico**: a transferência inteira acontece dentro da escrita que
a dispara, sem cobrar um ciclo sequer da CPU. A GPU também não cobra tempo de
desenho. Seis testes medem exatamente isso e falham todos pela mesma razão.

| Teste | O que mede |
| --- | --- |
| `dma/chopping` | Ciclos que o DMA rouba da CPU por bloco — reportamos 0 |
| `gpu/bandwidth` | Vazão de transferências para a VRAM |
| `cpu/access-time` | Waitstates por região do mapa de memória |
| `cdrom/timing` | Latência de resposta dos comandos do drive |
| `timers` / `timer-dump` | Os contadores já batem exatamente; o que falha é o custo do DMA aparecer como zero |
| `dma/chain-looping` | O comportamento observável já bate; sobra o custo em ciclos |

**Ajuste:** um scheduler que intercale DMA e CPU, cobrando ciclos por bloco
transferido. É a mudança estrutural mais cara da lista e resolve os seis de uma
vez. **Fora do escopo do MVP** — mas vale registrar que essa é a fronteira
entre "roda o jogo" e "cycle-accurate".

Há uma ressalva legítima: se a trava do Xenogears for de timing, este item sobe
para o topo da lista.

---

## 5. Subsistemas incompletos

### SPU — só o esqueleto

O bloco responde a leituras e escritas nos registradores, mas **não há mixagem**.
Falta: as 24 vozes, decodificação ADPCM, envelope ADSR, pitch e volume por voz,
reverb, IRQ e captura. O teste `spu/memory-transfer` falha no caminho de
transferência da SPU RAM.

É o maior bloco de trabalho restante em linhas de código, e o único item da
lista que é implementação nova em vez de correção.

### MDEC — decodifica certo, arredonda diferente

O decodificador e o DMA de saída funcionam. Comparado ao console em 8 bpp, a
maioria dos bytes bate exatamente e alguns divergem em 1 ou 2 unidades. É
arredondamento dentro do IDCT, não erro estrutural — bom o bastante para FMV.

Ficar bit a bit igual exige descobrir o arredondamento exato que o silício usa
nos deslocamentos internos. **Não é bloqueante para o MVP.**

---

## 6. O rastro diferencial — metade construída

A ideia registrada no plano: gravar cada acesso a registrador de I/O a partir do
frame em que o jogo assume o controle, e comparar com o mesmo rastro de um
emulador de referência que roda o jogo. A primeira divergência é a causa.

**Nossa metade está pronta e commitada** (`83ad0a5`): o ring buffer em
[bus.rs:88](../crates/psx-core/src/bus.rs#L88) e o exemplo
[iotrace.rs](../crates/psx-core/examples/iotrace.rs).

**A metade da referência não foi feita.** Nenhum emulador disponível emite um
log de I/O comparável já pronto:

- **DuckStation** tem release x64 para Windows, mas não exporta log de I/O.
- **Avocado** (do mesmo autor do ps1-tests, o que o torna o mais próximo do
  nosso vocabulário de testes) não tem release publicada.

Qualquer um dos dois exige compilar C++ com instrumentação própria. É factível,
mas é uma empreitada em si — decisão em aberto, dependente de a instrumentação
do `DMA3_CHCR` (seção 2) resolver ou não a trava sozinha.

Se for para fazer: **Avocado é a escolha**, por ser o mais simples de
instrumentar.

---

## 7. Fora do escopo do MVP

Registrados para não serem redescobertos como se fossem bugs novos:

- `cdrom/disc-swap` — exige um humano abrir a tampa; não automatizável sem um
  gatilho sintético no harness.
- Memory card.
- CD-DA e XA (áudio do disco).
- Bit-exatidão do MDEC (seção 5).
- Timing cycle-accurate de barramento (seção 4).

---

## 8. Ordem sugerida de ataque

1. **Instrumentar o laço do `DMA3_CHCR`** — o sinal mais forte que temos, e o
   mais barato de perseguir.
2. Depois, **`cpu/code-in-io`** e **`cpu/io-access-bitwidth`** — pequenos e
   independentes.
3. **`cdrom/getloc`**, medindo pelo total de casos.
4. **SPU** — o bloco grande, mas previsível.
5. Só então decidir sobre o emulador de referência instrumentado ou o scheduler
   de ciclos, conforme o que ainda estiver aberto.

---

## Apêndice — classes de erro já corrigidas

Cada uma apareceu mais de uma vez durante o desenvolvimento. Ficam registradas
como padrão a vigiar.

| Classe | Onde apareceu |
| --- | --- |
| Registrador de 16 bits tratado como 32 | GTE: truncar **na escrita**, não na leitura |
| Saturação calculada a partir de valor já truncado | GTE: `SX2`/`SY2`/`OTZ` vinham do MAC0 truncado em vez do resultado cheio |
| Overflow checado em bloco em vez de por estágio | GTE: cada produto do multiplicador-acumulador tem sua própria flag |
| Escrever direto no acumulador engolindo flags | GTE: `apply_vertex_color` |
| Coprocessador habilitado deve virar no-op, não exceção | CPU: COP1/COP3 |
| Checagem de `SR.CU` faltando | CPU: `LWC2`/`SWC2` |
| Bit de status sem checar a permissão que o governa | GPU: bit 15 do `GPUSTAT` e o `GP1(0x09)` |
| Máscara de escrita ignorada | GPU: `GP0(0xE6)` na transferência CPU→VRAM |
| Significado do bit invertido | GPU: bit 27 é "há dado esperando", não "GPU ociosa" |
| Canal cabeado tratado como configurável | DMA: canal 6 (OTC) ignora direção, passo e sync |
| Sinal simultâneo que na verdade é atrasado | SIO0: `/ACK` chega ~338 ciclos depois |
| Avanço em bloco onde o hardware avança por instrução | Timers, e depois o CD-ROM |
| Divisor inteiro onde a razão é fracionária | Timers: dot clock é 11/7, não 11÷7 |
| Escrita estreita perdendo os bytes altos | Bus: o barramento não tem byte-enables |
| Cadeia de DMA auto-referente sem limite | Bus: `MAX_LINKED_LIST_NODES` |

---

## Restrição permanente

BIOS e imagens de jogo são material protegido por direitos autorais e **nunca**
podem ser commitados ou distribuídos. O `.gitignore` bloqueia `/bios/` e
`/games/` por diretório, não por extensão. Auditar os objetos do git antes de
cada push.
