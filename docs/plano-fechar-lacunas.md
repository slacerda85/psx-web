# Plano para fechar as lacunas

Plano derivado de `prompt-agente-lacunas-psx.md`, com cada afirmação conferida
contra o PSX-SPX e contra o nosso código antes de virar tarefa. Onde o prompt
está certo, isto diz onde a fonte confirma. Onde precisa de ajuste, diz por quê.

Referências: [lacunas-e-incertezas.md](lacunas-e-incertezas.md),
[erros-e-ajustes.md](erros-e-ajustes.md),
[divergencias-do-console.md](divergencias-do-console.md).

---

## O que foi conferido antes de planejar

| Afirmação do prompt | Veredito |
| --- | --- |
| `current_block` do MDEC está fixo em 4 | **Confirmado.** `status \|= 4 << 16` em `mdec/mod.rs` |
| O campo alimenta o reordenamento do DMA1 | **Confirmado pela fonte**, citação abaixo |
| Bit 27 desce depois das primeiras palavras | **Confirmado pela fonte** |
| Buffer do CD tem 2 slots alcançáveis | **Confirmado**, já estava em lacunas §1.1 |
| Fórmula do ruído da SPU está errada | **Confirmado**, a nossa não veio de fonte nenhuma |
| IRQ da SPU dispara em qualquer acesso à RAM | **Confirmado pela fonte** |
| `GPUSTAT.14` espelha `GP1(08h).7` | **Parcialmente.** Ver correção 1 |
| A causa da trava dos três jogos está no MDEC | **Não sustentado.** Ver correção 2 |

O MDEC, na fonte:

> *"If there's data in the output fifo, then the Current Block bits are always
> set to the current output block number (...) **this information is apparently
> passed to the DMA1 controller, so that it knows if and how it must re-order
> the data in RAM**. If the output fifo is empty, then the bits indicate the
> currently processed incoming block."*

> *"[bit 27] gets set when a block is available, but, **it gets cleared after
> reading the first some words of that block** (nethertheless, one can keep
> reading the whole block, until the fifo-empty flag gets set)."*

---

## Correção 1 — `GPUSTAT.14` no console retail

O prompt diz que o bit 14 é o *reverseflag* e que a GPU v2 "distorce". A tabela
de versões do PSX-SPX diz outra coisa para o hardware que emulamos:

```
  Differences...                v0            v1 (protótipo)   v2 (retail)
  GPUSTAT.13 when interlace=off always 0      unknown          always 1
  GPUSTAT.14                    always 0      screen flip      nonfunctional screen flip
  GPUSTAT.15                    always 0      always 0?        bit1 of texpage Y base
```

No retail o bit 14 é **legível e sem efeito visual**. Espelhá-lo do `GP1(08h)`
está certo; renderizar qualquer distorção seria errado.

E a mesma tabela mostra **duas divergências que o prompt não pegou**, ambas no
hardware que emulamos:

- **`GPUSTAT.13` com entrelaçamento desligado é sempre 1 no v2.** Nós o
  derivamos de `odd_field`, que é zero fora do entrelaçamento. Um jogo que leia
  esse bit vê o valor de um console que não existe mais.
- **`GPUSTAT.15` é o bit 1 da base Y da texpage no v2.** Provavelmente
  reportamos zero.

Os dois entram no plano, na fase da GPU.

---

## Correção 2 — a Fase 1 do prompt parte de uma premissa que medi e não bate

O prompt trata o MDEC como a causa provável da trava dos três jogos, e manda
instrumentar `DMA1_CHCR` para provar.

**Eu já fiz essa medição.** No Guilty Gear, numa janela de 400 mil acessos de
I/O, o MDEC aparece assim:

```
  2 escritas em MDEC_CTRL   (reset, depois habilita DMA in/out)
  2 escritas em MDEC_CMD    (tabela de quantização, tabela do IDCT)
  3 escritas em DMA0_CHCR   (as tabelas indo por DMA)
  1 escrita  em DMA1_CHCR   (valor 0 — desliga o canal)
  9 leituras de DMA1_CHCR   (o laço principal consultando)
```

O jogo **nunca envia um comando de decodificação**. O MDEC não produz saída, o
DMA1 nunca carrega dado, e `current_block` nunca é consultado por ninguém que
esteja reordenando um bloco. A hipótese não se sustenta para este jogo.

O próprio prompt prevê isso: *"Se a Fase 1 item 1.1 mostrar uma causa DIFERENTE
das hipóteses A/B, NÃO force 1.2/1.3."*

**Consequência para o plano:** as correções do MDEC continuam valendo — são
erros reais com fonte — mas saem da posição de "prioridade absoluta" e viram
uma fase normal. A prioridade passa para o buffer do CD, que é onde o sintoma
mora.

---

## As fases

Cada uma é independente. Cada correção cita a seção da fonte no comentário do
código, ganha teste que falha antes e passa depois, e fecha o item
correspondente em `lacunas-e-incertezas.md`.

### Fase 1 — Buffer do CD com dois slots (lacunas §1.1)

**Por que primeiro:** é a única lacuna aberta que fica no caminho exato do
sintoma. Os três jogos travados param de recolher setores; o modelo de buffer é
o que decide qual setor está disponível quando eles voltam a pedir.

O que a fonte descreve: SRAM de 32 KiB em 8 slots, dos quais o controlador só
entrega **dois** — o mais antigo, que a INT1 aponta, e o mais novo, que o
`GetlocL` lê. Depois do acknowledge ele **pula para o mais novo**, e os do meio
somem sem bandeira de erro.

Substituir o `staged_sector` único por:

```
oldest  — o que a INT1 entrega e o BFRD trava
newest  — o que o GetlocL lê
```

Regras a implementar, todas da fonte:

1. Setor produzido: `newest = setor`; se `oldest` está vazio, `oldest = newest`.
2. INT1 só existe com `oldest` preenchido e ainda não entregue.
3. No acknowledge da INT1: `oldest = newest`.
4. `GetlocL` lê o header do `newest`; sem header, INT5 razão `0x80`.
5. `GetlocP` não usa os slots — vem da posição mecânica, e funciona durante seek.
6. `BFRD` **trava** o `oldest`: um overrun depois dele não corrompe o setor em
   trânsito, e um overrun antes faz o jogo ler outro setor sem erro nenhum.

**Testes:** produzir 0,1,2,3 sem acknowledge e verificar que o próximo INT1 é o
3, não o 1; `GetlocL` no meio devolve o header do 3; `GetlocL` durante seek dá
INT5; `BFRD` no 0 seguido de 1..8 ainda entrega o 0.

**Medir:** `hwtest --only cdrom/getloc` pelo **total** de casos. Já houve
regressão aqui ao condicionar o `GetlocP` a um flag caseiro — não repetir.

**Aceite:** o total do `getloc` sobe ou fica igual; os quatro jogos medidos
antes e depois.

### Fase 2 — `BFRD` e DMA3 curto (lacunas §1.2, §1.3, §3.2)

O prompt resolve bem a dúvida do `HXFR` que eu tinha deixado aberta: é interno
do CXD1815, programado pelo microcontrolador, **invisível à CPU**. Não há o que
modelar — só o efeito.

- `BFRD=1` com a transferência anterior terminada: copia `oldest` para a FIFO,
  cursor a zero, `DRQSTS=1`.
- `BFRD=1` no meio de uma transferência: **ignorar**.
- `BFRD=0`: esvazia a FIFO e cancela o resto.
- `DRQSTS` = armado **e** cursor antes do fim.

E o DMA3, que hoje lê zeros depois do fim da FIFO: transferir o que há e
**repetir o último byte válido** como enchimento, conforme o `RDDATA` do
PSX-SPX. O canal não pode ficar ocupado para sempre esperando dado.

**Teste:** FIFO de 4 bytes com BCR de 512 palavras conclui, `busy=0`, e a RAM
contém o enchimento.

### Fase 3 — MDEC: `current_block` e bit 27 (lacunas §4.2)

Erros reais, com fonte, sem sintoma conhecido nos nossos jogos. Depois do
buffer do CD porque a medição não os aponta como causa.

- `current_block` passa a ser um índice 0..=5. Com a FIFO de saída vazia,
  reporta o bloco de **entrada**; com dado na FIFO, o de **saída**. Mono
  sempre 4.
- Bit 27 sobe quando um bloco fica pronto e o DMA de saída está habilitado, e
  **desce depois das primeiras palavras lidas** — com o resto ainda legível até
  a FIFO esvaziar. O limiar exato não está na fonte; escolher um curto e
  **documentá-lo como escolha, não como fato**.

**Cuidado medido:** o reset do MDEC deixa o status em `0x80040000`, que tem
`current_block = 4`. É por isso que o nosso valor fixo casa com o
`cpu/io-access-bitwidth` hoje. O teste desse caso não pode regredir.

### Fase 4 — SPU: IRQ, ruído e sweep (lacunas §2.1, §2.4, §2.5, §2.6)

**IRQ de endereço.** A fonte é explícita e fecha duas lacunas de uma vez:

> *"all voices are permanently reading data from SPU RAM — even in Noise mode,
> even if the Voice Volume is zero, and even if the ADSR pattern has finished
> the Release period — so even inaudible voices can trigger IRQs."*

> *"Setting the IRQ address to 0000h..01FFh will trigger IRQs on writes to the
> four capture buffers."*

Concentrar todo acesso à SPU RAM em `ram_read`/`ram_write` e fazer a checagem
lá dentro. Isso cobre voz, captura e DMA4 de uma vez, e resolve a §2.5 sem
palpite. A §2.6 (quantos blocos o decodificador lê à frente) some junto: a IRQ
passa a ser na busca do bloco.

A fonte também alinha o teste: *"For stable IRQs, the IRQ address should be
aligned to the 16-byte ADPCM blocks"* — endereço no meio de um bloco é
instável no silício, então o teste usa múltiplos de 16.

**Ruído.** A nossa fórmula não veio de lugar nenhum. A da fonte, a 44,1 kHz:

```
  Timer -= NoiseStep                       ; passo 4..7
  Parity = bit15 xor bit12 xor bit11 xor bit10 xor 1
  se Timer < 0: NoiseLevel = NoiseLevel*2 + Parity
  se Timer < 0: Timer += 0x20000 >> NoiseShift   ; recarrega
  se Timer < 0: Timer += 0x20000 >> NoiseShift   ; de novo se preciso
```

**Sweep.** Reusar a máquina do ADSR que já existe, partindo do volume atual.
Hoje devolvemos meia escala fixa, o que é audível e errado.

**Dois achados extras da mesma página**, que entram nesta fase:

- `IRQ9 Enable (only when Bit15=1)` — a IRQ da SPU só vale com a SPU ligada.
  Não checamos o bit 15.
- *"Changes to bit0-5 aren't applied immediately; after writing to SPUCNT, wait
  until the LSBs of SPUSTAT are updated"* — o `SPUSTAT` **atrasa** em relação ao
  `SPUCNT`. Nós espelhamos na hora. Baixo risco (o software não trava), mas é
  divergência; documentar em vez de implementar sem medida.

### Fase 5 — Seek por distância (lacunas §1.5, §1.6)

Hoje o seek é fixo em 200 mil ciclos. A fórmula do prompt vem do
Mednafen/DuckStation, não do PSX-SPX — então entra **marcada como tal** no
comentário, e o item continua em `lacunas-e-incertezas.md` com a procedência
trocada de "sem fonte" para "fonte secundária, sem medição própria".

Sem jitter aleatório. O `cdrom/timing` mede mínimo e máximo, não a média, e vai
continuar falhando; o critério é **não piorar o total**.

### Fase 6 — GPU (lacunas §5.2, mais os dois achados novos)

- `GPUSTAT.14` espelha `GP1(08h).7`, sem efeito visual no retail.
- `GPUSTAT.13` é **sempre 1** fora do entrelaçamento no v2.
- `GPUSTAT.15` é o bit 1 da base Y da texpage no v2.

Os três são leitura de registrador, com o `cpu/io-access-bitwidth` como
referência — ele já compara `GPUSTAT` contra o console, e hoje diferimos ali.

### Fase 7 — Fora desta passada

Registrados, não implementados: interpolação gaussiana (§2.2), reverb (§2.3),
bit-exatidão do IDCT (§4.1), scheduler de ciclos de DMA e GPU (§3.1, §5.1),
descarte da segunda resposta além do `Stop` (§1.4).

Sobre o §1.4: o prompt manda implementar o caso do `Stop`. **Sugiro não fazer
agora.** A fonte documenta um comando entre nove que têm segunda resposta;
implementar só ele é uma regra pela metade, e nenhum dos quatro jogos de teste
usa `Stop` nessa sequência. O ganho é zero e o risco de divergir mais é real.

---

## Ordem, e por que esta

1. **Buffer do CD com dois slots** — a única lacuda no caminho do sintoma.
2. **`BFRD` e DMA3 curto** — mesmo subsistema, depende do buffer estar certo.
3. **MDEC `current_block` e bit 27** — erro real, sem sintoma medido.
4. **SPU: IRQ na RAM, ruído, sweep** — fecha quatro lacunas de uma vez.
5. **Seek por distância** — troca a procedência do item, não fecha.
6. **GPU: bits 13, 14 e 15 do `GPUSTAT`** — barato, com teste de hardware.

Depois de cada fase: `cargo test --workspace`, `clippy -D warnings`,
`fmt --check`, os quatro jogos medidos, e o `hwtest` do subconjunto afetado.
GTE tem que continuar em 1150/1150 e a BIOS em 99,6%.

---

## O que este plano **não** promete

Nenhuma das seis fases tem evidência de destravar Guilty Gear, Grandstream Saga
ou Xenogears. A Fase 1 é a mais provável porque fica no caminho do sintoma, não
porque alguma medição a aponte.

Oito hipóteses já foram testadas e descartadas para essa trava (tabela em
`lacunas-e-incertezas.md`). Se a Fase 1 não mudar nada, o próximo passo não é a
Fase 2 — é voltar a medir, com o alvo declarado: **descobrir o que faz o laço
principal do Guilty Gear, em `0x80066Dxx`, decidir pedir ou não pedir o
payload.** Só 19 pedidos em 1500 frames, e quando pede, funciona.

---

## Registro de execução

### Fase 1 (buffer de dois slots) — implementada e **revertida**

Foi escrita por inteiro, com os três testes que o plano pedia, e os três
passaram: o salto para o mais novo, a leitura sequencial sem perda quando o
software acompanha, e o travamento do setor pelo `BFRD`.

**Quebrou o boot.** Com o descarte ligado, o BIOS deixa de ler `SYSTEM.CNF`
corretamente e cai no `cdrom:PSX.EXE;1` de fallback — nenhum dos quatro jogos
carrega. Ele recolhe 8 dos 10 setores do boot e perde dois.

O descarte é correto no console. O que falta aqui é o resto do relógio: as
nossas latências de comando são médias, a ordenação das respostas é uma fila
por tempo em vez das duas bandeiras do silício, e a velocidade da CPU é
aproximada. Descartar setores fielmente dentro desse conjunto de aproximações
destrói dado que o console real teria entregue.

**Dependência descoberta:** o buffer de dois slots precisa do modelo de
respostas por bandeira (§11 de [divergencias-do-console.md](divergencias-do-console.md))
antes de ser seguro. A ordem certa é o inverso da que este plano propôs.

### Fase 3 (MDEC) — não implementada, com motivo

O `current_block` não tem como ser fiel na nossa arquitetura. O campo existe
para o DMA1 saber **como reordenar** o bloco na RAM, e o nosso MDEC monta o
macrobloco inteiro em `emit_color` antes de enfileirar a saída — o
reordenamento que o campo serve para guiar já aconteceu. Reportar um índice
variável seria decoração, não emulação.

Implementá-lo de verdade significa emitir bloco a bloco e mover o
reordenamento para o canal 1 do DMA. É refatoração real, e nenhum dos quatro
jogos lê o campo.

O bit 27 tem o mesmo problema em menor escala: a fonte diz que ele desce
*"after reading the first some words"*, sem dizer quantas. Escolher um limiar
seria inventar um número.

Os dois continuam abertos, agora com o motivo registrado.
