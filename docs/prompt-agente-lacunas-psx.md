# Prompt para o agente — fechar lacunas e incertezas do psx-web

Cole o bloco abaixo como instrução de sistema / tarefa no VS Code (Copilot / Cursor / Claude).
Não pular a Fase 0. Não commitar BIOS nem ISOs.

Referências internas (ler antes de mexer em código):

- `docs/lacunas-e-incertezas.md` — o que está aproximado
- `docs/erros-e-ajustes.md` — bugs abertos e ferramentas
- `docs/pendencias-hardware.md` — placar contra o console

Fonte de verdade de hardware: https://psx-spx.consoledev.net/

Ferramentas já existentes em `crates/psx-core/examples/`:

- `hwtest`, `iotrace`, `hotspots`, `screenshot`

---

```
Você é o agente de correção de hardware do emulador psx-web
(https://github.com/slacerda85/psx-web).

Objetivo desta passada: fechar as lacunas documentadas em
docs/lacunas-e-incertezas.md com o comportamento que o PSX-SPX
(e, onde o SPX é omisso, DuckStation/Mednafen) descrevem.
Não inferir em silêncio. Cada correção cita a seção do SPX
(ou o commit/arquivo de referência) no comentário do código
e no docs/lacunas-e-incertezas.md (o item SAI do arquivo
quando houver fonte + teste).

Restrição permanente: nunca commitar /bios/ nem /games/.


══════════════════════════════════════════════════════════════════
FASE 0 — REGRAS E PORTÕES
══════════════════════════════════════════════════════════════════

1. Ler por inteiro, nesta ordem:
   - docs/lacunas-e-incertezas.md
   - docs/erros-e-ajustes.md
   - docs/pendencias-hardware.md
   - crates/psx-core/src/cdrom/mod.rs
   - crates/psx-core/src/mdec/mod.rs
   - crates/psx-core/src/dma/mod.rs
   - crates/psx-core/src/spu/mod.rs
   - crates/psx-core/src/gpu/mod.rs

2. Toda correção ganha teste unitário em crates/psx-core que
   FALHA antes e PASSA depois. Sem teste, a correção não existe.

3. Depois de cada fase:
     cargo test --workspace
     cargo clippy --workspace --all-targets -- -D warnings
     cargo fmt --all --check
   Nenhum teste que hoje passa pode falhar.

4. Quando o hwtest estiver disponível:
     cargo run --release -p psx-core --example hwtest -- \
       --bios bios/SCPH1001.BIN --tests <ps1-tests> \
       --only <teste> --verbose

5. NÃO implementar nesta passada:
   - scheduler cycle-accurate completo (dma/chopping, gpu/bandwidth,
     cpu/access-time) — só um gancho mínimo se a Fase 1 provar
     que GPUSTAT.26/28 infinitos travam um dos três jogos
   - interpolação gaussiana da SPU (deixar para Fase 7)
   - reverb completo da SPU (deixar para Fase 7)
   - bit-exatidão do IDCT do MDEC (deixar para Fase 7)
   - memory card, DualShock analógico, CD-DA, XA
   - jitter aleatório nas latências do CD

6. Conventional Commits. Um tema por commit. Atualizar
   docs/lacunas-e-incertezas.md no commit que fecha o item
   (mover para um apêndice "fechados" com a referência).


══════════════════════════════════════════════════════════════════
FASE 1 — QUEBRAR O CÍRCULO (Guilty Gear / Grandstream / Xenogears)
Prioridade absoluta. Não mexer no buffer do CD antes de ter o log.
══════════════════════════════════════════════════════════════════

Sintoma documentado: o drive entrega setores; o jogo reconhece
as IRQs, lê os 8 bytes de cabeçalho e NÃO pede o payload.
O laço principal (Guilty Gear em 0x80066Dxx) espera DMA1_CHCR
(MDEC out). Sem decode não há dados; sem dados o jogo não pede
o próximo setor. Circular.

As oito hipóteses já testadas e revertidas NÃO cobrem o MDEC.
Candidatos que ainda não foram isolados:

  A. current_block do MDEC (status bits 18-16) está fixo em 4.
     PSX-SPX "MDEC Status Register":
       18-16 Current Block (0..3=Y1..Y4, 4=Cr, 5=Cb; mono: sempre 4=Y)
       Com FIFO de saída NÃO vazia: número do bloco que está
       SAINDO (Y1..Y4). Com FIFO vazia: bloco de ENTRADA
       (Cr=4, Cb=5, Y1..Y4=0..3).
     O DMA1 usa esse campo para reordenar o 8×8 na RAM.
     Reportar sempre 4 diz "estou no Cr" o tempo todo.

  B. Bit 27 do MDEC (Data-Out Request) está errado.
     SPX: sobe quando um bloco está pronto; DESCE depois das
     primeiras palavras lidas, mesmo com o resto ainda na FIFO.
     Dá para continuar lendo até fifo-empty. Se o bit 27 fica
     1 o tempo todo, ou 0 cedo demais, o jogo poleia DMA1.

  C. Demux STR não junta um frame (header 0x3800) porque o
     buffer do CD "segura" setores em vez de pular oldest→newest.
     Tratar DEPOIS do log. Não inverter a ordem.

Trabalho obrigatório, nesta ordem:

### 1.1 Instrumentar (commit: test(mdec): log DMA1 + status no hang)

Estender iotrace (ou um exemplo novo `mdectrace`) para, a cada
leitura/escrita de:

  - 0x1F8010C8  DMA1_CHCR
  - 0x1F801824  MDEC status / control
  - 0x1F801820  MDEC command / data
  - 0x1F8010B8  DMA3_CHCR   (só para cruzar)

registrar no MESMO ciclo:

  - PC
  - valor lido/escrito
  - MDEC: bit31 out-empty, bit29 busy, bit28 in-req, bit27 out-req,
    bits18-16 current_block, bits15-0 words remaining
  - último comando MDEC (0=nop, 1=decode, 2=iqtab, 3=scale)
  - CD: drive phase, sector_available, BFRD, LBA, INT pendente
  - DMA1 e DMA3: busy, trigger, sync mode

Rodar Guilty Gear (o mais legível) por 1500 frames:

  cargo run --release -p psx-core --example iotrace -- \
    --bios bios/SCPH1001.BIN --disc games/guilty-gear/<cue> \
    --frames 1500 --tail 80

Marcar o frame ~1000 em que enable do CD cai de 0x1F para 0x07
e a PRIMEIRA vez em que DMA1 fica busy sem current_block andar
4 → 5 → 0 → 1 → 2 → 3.

### 1.2 Corrigir current_block (commit: fix(mdec): current_block por estágio)

Em crates/psx-core/src/mdec/mod.rs:

  - Manter um índice 0..=5 do bloco em processamento.
    Ordem colorida: Cr=4, Cb=5, Y1=0, Y2=1, Y3=2, Y4=3
    (DuckStation usa 0=Cr,1=Cb,2-5=Y no array interno — o que
    IMPORTA é o valor reportado no status, não o índice do array).
  - Enquanto a FIFO de saída está vazia: reportar o bloco de
    ENTRADA que o decodificador está comendo.
  - Enquanto a FIFO de saída tem dados: reportar o bloco de
    SAÍDA que o DMA1 está lendo (Y1..Y4).
  - Mono 8 bpp: sempre 4.

Teste unitário:
  - Alimentar um macrobloco colorido mínimo.
  - Após o Cr: status bits 18-16 == 4 (FIFO vazia) ou o Y que
    estiver saindo (FIFO cheia).
  - Após completar Y4 e esvaziar a FIFO: volta a 4 no próximo Cr.
  - Nunca ficar preso em 4 durante Y1..Y4 de saída.

### 1.3 Corrigir bit 27 (commit: fix(mdec): data-out request desce cedo)

  - Bit 27 = 1 quando um bloco recém-completo é oferecido E
    DMA out está habilitado (control bit 29).
  - Depois que o host/DMA leu as PRIMEIRAS palavras do bloco
    (DuckStation: algumas words; use um limiar curto, p.ex.
    1..8 words, documentado no comentário), bit 27 = 0.
  - Bit 31 (fifo empty) continua refletindo a FIFO de verdade.
  - O resto do bloco ainda pode ser lido até empty=1.

Teste unitário:
  - Bloco pronto → bit27=1, bit31=0.
  - Ler 8 words → bit27=0, bit31=0.
  - Ler o resto → bit31=1.

### 1.4 Aceite da Fase 1

  - Log mostra current_block andando.
  - Guilty Gear: depois do frame 1000, o jogo VOLTA a pedir
    payload (BFRD + DMA3) OU o log aponta o próximo registrador
    (não chute outro subsistema).
  - Screenshot 2000+ frames: framebuffer diferente do freeze.
  - cargo test / clippy / fmt verdes.
  - GTE 1150/1150 e BIOS ≥99,6% intactos.


══════════════════════════════════════════════════════════════════
FASE 2 — BUFFER DO CD: DOIS SLOTS, PERDER O MEIO
docs/lacunas §1.1
══════════════════════════════════════════════════════════════════

PSX-SPX "CDROM Response/Data Queueing" e "Sector Buffer VS GetlocL":

  - SRAM 32 KiB / 8 slots físicos.
  - O HC05 só entrega 2: oldest (INT1) e newest (GetlocL).
  - Depois do ACK da INT1, PULA para o newest. Setores do meio
    somem sem flag de overrun.

Modelo a implementar em crates/psx-core/src/cdrom/mod.rs
(substituir a fila que segura tudo):

  struct SectorSlots {
      oldest: Option<Sector>, // o que a INT1 entrega + BFRD/DMA
      newest: Option<Sector>, // o que GetlocL lê
  }

Regras:

  1. A cada setor mecânico produzido:
       newest = Some(setor)
       se oldest.is_none() { oldest = newest.clone() }
  2. INT1 existe somente se oldest.is_some() e ainda não foi
     entregue nesta geração.
  3. No acknowledge da INT1:
       oldest = newest  (se forem o mesmo LBA, um slot só)
       se oldest == newest, o próximo setor mecânico é que
       preenche newest de novo
  4. GetlocL lê o HEADER do newest. Se newest.is_none()
     (seek, sem header): INT5 reason 80h.
  5. GetlocP NÃO usa esses slots; usa SubQ da posição mecânica.
     Funciona durante seek.
  6. Data Request (BFRD) TRAVA o oldest. Overrun DEPOIS do
     BFRD não corrompe esse setor (SPX "Incoming Data / Buffer
     Overrun"). Overrun ANTES do BFRD: oldest já foi
     substituído — o jogo lê o setor errado, sem erro.

Testes unitários:

  - Produzir setores 0,1,2,3 sem ACK: oldest=0, newest=3.
    ACK → oldest=3. Próximo INT1 é o 3, não o 1.
  - GetlocL no meio devolve header do newest (3), não do oldest.
  - GetlocL durante seek (newest sem header) → INT5 80h.
  - BFRD no oldest=0, depois chegam 1..8: DMA ainda lê o 0.

hwtest --only cdrom/getloc --verbose
Medir pelo TOTAL de casos. Já houve regressão ao condicionar
GetlocP a position_valid — não repetir.

Commit: fix(cdrom): two-slot buffer skips unread sectors


══════════════════════════════════════════════════════════════════
FASE 3 — BFRD / DRQSTS / DMA3 (docs/lacunas §1.2 §1.3 §3.2)
══════════════════════════════════════════════════════════════════

HXFR é interno do CXD1815, programado pelo HC05, invisível à
CPU. Não modelar o registrador. Modelar o efeito:

  - BFRD=1 com transferência anterior ACABADA (cursor >= len
    ou FIFO vazia): copia oldest → FIFO, cursor=0, DRQSTS=1.
  - BFRD=1 no MEIO de uma transferência: IGNORAR. O chip só
    relê se HXFRC==0.
  - BFRD=0: esvazia a FIFO, DRQSTS=0, cancela o restante.
  - DRQSTS = (BFRD armado) && (cursor < len).

DMA3 é SyncMode 0 (manual + trigger). NÃO há DRQ de dispositivo
que segure o canal.

  - Se a FIFO tem menos bytes que o BCR: transferir o que há
    e PADAR com o último byte válido.
      modo 800h: padding = byte[800h-8]
      modo 924h: padding = byte[924h-4]
    (PSX-SPX RDDATA)
  - NÃO deixar CHCR.busy=1 para sempre à espera de mais dados.
  - Bit 28 (trigger) desce no INÍCIO; bit 24 (busy) desce na
    CONCLUSÃO.

Testes:

  - BFRD no meio não troca o setor.
  - DMA3 com FIFO de 4 bytes e BCR de 512 words: termina,
    busy=0, RAM contém padding do último byte.
  - DMA3 com FIFO cheia de 2048 bytes: 512 words corretas.

Commit: fix(cdrom): BFRD ignores in-flight; DMA3 pads short FIFO


══════════════════════════════════════════════════════════════════
FASE 4 — SEGUNDA RESPOSTA (docs/lacunas §1.4)
══════════════════════════════════════════════════════════════════

PSX-SPX "BUSYSTS flag":

  Stop  + (INT3 ACK) + comando novo  → DESCARTA a INT2 do Stop.
  Pause / ReadN / ReadS              → NÃO descarta.

Caso extra documentado nos testes do próprio SPX:
  Pause + delay + GetlocL pode perder a INT2 do Pause.
  Implementar SÓ se o getloc do ps1-tests exigir. Senão, deixar
  anotado em lacunas-e-incertezas.md como "observado no SPX,
  não coberto por teste nosso".

Implementar agora:

  - Fila de segunda resposta marcada com o opcode que a gerou.
  - Se opcode==Stop e chega comando novo após INT3 ter sido
    reconhecida e antes da INT2: drop da INT2, executa o novo.
  - NÃO generalizar para Seek/Init/GetID/MotorOn/ReadTOC.

Teste unitário: Stop → INT3 → ack → Getstat → NÃO há INT2 do
Stop; há INT3 do Getstat.

Commit: fix(cdrom): Stop drops pending INT2 on next command


══════════════════════════════════════════════════════════════════
FASE 5 — SEEK POR DISTÂNCIA, SEM JITTER (docs/lacunas §1.5 §1.6)
══════════════════════════════════════════════════════════════════

Não inventar variação aleatória. Usar a fórmula Mednafen /
DuckStation (commit 74013a08 / 05e4e7d2):

  Δ = abs(lba_alvo - lba_atual)   // 0 se motor off, lba_atual=0

  ticks = max(20_000,
              Δ * CPU_CLOCK * 1000 / (72 * 60 * 75) / 1000)
  se motor_off:            ticks += CPU_CLOCK          // ~1 s
  se paused && Δ pequeno:  ticks += 1_237_952 * (1x? 2 : 1)
  se Δ >= 2550:            ticks += CPU_CLOCK * 300 / 1000
  se mudou 1x↔2x:          ticks += CPU_CLOCK * 3 / 2

Pause (SPX Response Timings):
  lendo 1x:  0x0021_181C
  lendo 2x:  0x0010_BD93
  já paused: 0x0000_1DF2

ACK dos comandos curtos: manter a média já medida
(ACKNOWLEDGE_DELAY). Init/ReadTOC continuam com delay longo
próprio.

cdrom/timing PODE continuar falhando (ele mede min/max, não a
média). Não piorar o total. Não adicionar RNG.

Teste unitário: seek de 0→100 LBA < seek de 0→20000 LBA.
Pause-when-paused << Pause-when-reading.

Commit: fix(cdrom): seek latency scales with LBA distance


══════════════════════════════════════════════════════════════════
FASE 6 — SPU: IRQ NA RAM, RUÍDO, SWEEP
docs/lacunas §2.1 §2.4 §2.5 §2.6
══════════════════════════════════════════════════════════════════

### 6.1 IRQ de endereço (fechar §2.5 e §2.6 juntos)

PSX-SPX "SPU Interrupt":

  - Dispara quando UMA VOZ LÊ o endereço programado em
    0x1F801DA4 (valor em unidades de 8 bytes).
  - Vozes inaudíveis, em noise, volume 0 ou ADSR terminado
    TAMBÉM leem. Não dá para pular a voz.
  - 0x000..0x1FF (byte 0x00000..0x00FFF): IRQ nas ESCRITAS
    dos quatro capture buffers.
  - Reverb também dispara se o endereço cai na work area.
  - Endereço no MEIO de um bloco de 16 bytes é instável no
    silício; alinhar os testes a múltiplos de 16.

Implementação:

  - Funções ram_read(addr) e ram_write(addr, val) únicas.
  - Dentro delas: se IRQ enable (SPUCNT bit 6) e
    (addr & !7) == (irq_addr << 3): sobe flag em SPUSTAT
    e IRQ9.
  - Capture e DMA4 passam por essas funções.
  - Decoder: 1 bloco (28 amostras / 16 bytes) por vez, quando
    o pitch counter cruza o fim das 28. IRQ no FETCH do bloco,
    não na amostra. Esquecer "11 amostras à frente".

Teste: escrever irq_addr no start de um bloco, key-on a voz,
avançar 28 amostras, assert IRQ. DMA4 que passe pelo endereço
também dispara.

### 6.2 Ruído (§2.4) — a fórmula atual está ERRADA

Substituir
  period = (0x8000 >> shift).max(1) * (4 + step)
pela máquina do SPX "SPU Noise Generator":

  a cada sample 44.1 kHz:
    Timer -= NoiseStep          // 4..7 conforme o campo step
    Parity = bit15 xor bit12 xor bit11 xor bit10 xor 1
    se Timer < 0:
        NoiseLevel = NoiseLevel*2 + Parity
        Timer += 0x20000 >> NoiseShift
        se Timer < 0: Timer += 0x20000 >> NoiseShift   // até 2x

Teste: shift/step conhecidos produzem sequência determinística
de pelo menos 32 bits. Comparar com Mednafen se houver vetor;
senão, travar o vetor nosso como regressão.

### 6.3 Sweep (§2.1)

Reusar a máquina do ADSR já existente (shift, step, linear/exp,
direção). Bit 15 do volume L/R liga o sweep. Começa no volume
ATUAL. Há 1 sample (44.1 kHz) de atraso entre gravar volume
fixo (bit15=0) e gravar o sweep (bit15=1) — o SPX avisa.

Não entrar em pânico como o rustation-ng. Não deixar meia
escala fixa.

Teste: volume 0x2000, sweep increase linear, após N ticks o
nível atual > 0x2000 e < 0x7FFF.

NÃO implementar gaussiana nem reverb nesta fase.

Commits:
  fix(spu): IRQ on every SPU RAM access
  fix(spu): noise LFSR matches SPX timer
  fix(spu): volume sweep uses ADSR envelope engine


══════════════════════════════════════════════════════════════════
FASE 7 — GPUSTAT.14 E POLIMENTO
docs/lacunas §5.2 (e §5.1 só se o log da Fase 1 pedir)
══════════════════════════════════════════════════════════════════

### 7.1 Bit 14 do GPUSTAT

NÃO é "não usado". É o reverseflag = GP1(08h) bit 7, espelhado
em GPUSTAT.14. GPU v1 vira a tela; GPU v2 (retail) distorce.
Não precisa renderizar o distúrbio. Precisa LER e ESCREVER
o bit.

  - GP1(08h) bit 7 grava o flag.
  - GPUSTAT bit 14 devolve o flag.
  - GP1(00h) reset zera o flag (SPX: display mode 320x200 NTSC).

Teste: GP1(08h) com bit 7 set → leitura de GPUSTAT tem bit 14.
Reset → bit 14 limpo.

Commit: fix(gpu): GPUSTAT.14 mirrors GP1(08h).7

### 7.2 Custo de desenho (§5.1)

SÓ se a Fase 1 mostrou um jogo poleando GPUSTAT.26/28 no freeze.
Nesse caso: baixar bit 26 por K * pixels do primitivo ciclos
(heurística curta, documentada) e subir de novo. Não construir
o scheduler dos seis testes do ps1-tests.

### 7.3 Fora desta passada (anotar, não implementar)

  - Gauss 4 pontos + tabela de 512 do SPX (§2.2)
  - Reverb IIR/APF do SPX (§2.3)
  - IDCT bit-exact via mdec/step-by-step-log (§4.1)
  - Scheduler DMA/CPU (§3.1)
  - Jitter do mainloop do HC05 (§1.5)


══════════════════════════════════════════════════════════════════
FASE 8 — REGRESSÃO E DOCUMENTAÇÃO
══════════════════════════════════════════════════════════════════

Rodar e colar o resumo no final da sessão:

A. Internos
     cargo test --workspace
     cargo clippy --workspace --all-targets -- -D warnings
     cargo fmt --all --check

B. hwtest (se o ambiente tiver bios + ps1-tests)
     --only cpu/code-in-io
     --only cpu/io-access-bitwidth
     --only cdrom/getloc
     --only spu/memory-transfer
     GTE não pode regredir.

C. Jogos
     screenshot --frames 2500 nos três:
       Guilty Gear, Grandstream Saga, Xenogears
     Aceite: framebuffer muda depois do ponto em que antes
     paravam de pedir payload. iotrace do DMA1 deixa de ser
     um busy eterno com current_block==4.

D. Documentação, último commit (docs: close resolved lacunae)

   Para cada item de lacunas-e-incertezas.md:

     FECHADO  → mover para seção "Fechados nesta passada"
                com: comportamento, fonte SPX, teste que cobre.
     ABERTO   → deixar no lugar com UMA frase do que ainda
                falta (ex.: "gaussiana, tabela no SPX, sem
                dependência de jogo").

   Atualizar erros-e-ajustes.md se a trava dos três jogos
   mudou de status.


══════════════════════════════════════════════════════════════════
ORDEM DE COMMITS
══════════════════════════════════════════════════════════════════

 1. test(mdec): log DMA1 + MDEC status no hang
 2. fix(mdec): current_block reflects in/out stage
 3. fix(mdec): data-out request clears after first words
 4. fix(cdrom): two-slot buffer skips unread sectors
 5. fix(cdrom): BFRD ignores in-flight; DMA3 pads short FIFO
 6. fix(cdrom): Stop drops pending INT2 on next command
 7. fix(cdrom): seek latency scales with LBA distance
 8. fix(spu): IRQ on every SPU RAM access
 9. fix(spu): noise LFSR matches SPX timer
10. fix(spu): volume sweep uses ADSR envelope engine
11. fix(gpu): GPUSTAT.14 mirrors GP1(08h).7
12. docs: close resolved lacunae

Se a Fase 1 item 1.1 mostrar uma causa DIFERENTE das hipóteses
A/B, NÃO force 1.2/1.3. Corrija o que o log mostrou e documente
a hipótese descartada em erros-e-ajustes.md.


══════════════════════════════════════════════════════════════════
CRITÉRIO DE ACEITE DESTA PASSADA
══════════════════════════════════════════════════════════════════

Não chamar de "emulação perfeita". Alvo mensurável:

  - Testes internos não regridem (só aumentam).
  - current_block do MDEC varia 4/5/0/1/2/3 conforme o estágio.
  - Buffer do CD tem 2 slots e GetlocL lê o newest.
  - Stop descarta INT2; Pause/ReadN não.
  - DMA3 não fica busy eterno com FIFO curta.
  - SPU IRQ dispara em ram_read/ram_write; ruído usa o LFSR
    do SPX; sweep anda.
  - GPUSTAT.14 espelha GP1(08h).7.
  - Guilty Gear / Grandstream / Xenogears saem do estado
    "IRQ reconhecida, payload nunca pedido", OU o log da
    Fase 1 aponta o próximo registrador com evidência.

Comece pela Fase 1 item 1.1. Sem o log, qualquer fix de
current_block ou de buffer do CD é chute.
```

---

Fim do prompt copiável.
