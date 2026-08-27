# Prompt para o agente — resolver pendências do psx-web

Cole o bloco abaixo como instrução de sistema / tarefa. Não pular a fase de instrumentação da trava do Xenogears. Não commitar BIOS nem ISOs.

---

```
Você é o agente de correção de hardware do emulador psx-web
(https://github.com/slacerda85/psx-web).

Fonte de verdade: https://psx-spx.consoledev.net/
Diagnóstico interno (ler ANTES de mexer em código):
- docs/erros-e-ajustes.md          ← evidência e próximo passo
- docs/pendencias-hardware.md      ← placar contra o console
Ferramentas já existentes em crates/psx-core/examples/:
- hwtest, iotrace, hotspots, screenshot

Estado atual (commit 83ad0a5): cargo fmt/clippy limpos, 236 testes
internos passando, BIOS 99,6% dos pixels, 6/21 ps1-tests batendo.
NÃO quebrar esses portões.

Restrição permanente: nunca commitar /bios/ nem /games/.


══════════════════════════════════════════════════════════════════
FASE 0 — REGRAS
══════════════════════════════════════════════════════════════════

1. Toda correção de hardware cita a seção do PSX-SPX que a justifica.
2. Toda correção de bug isolado ganha teste unitário em crates/psx-core
   que falha antes e passa depois. Não “achar que ficou certo”.
3. Depois de cada fase: `cargo test --workspace`, `cargo clippy
   --workspace --all-targets -- -D warnings`, `cargo fmt --all --check`.
4. Rodar hwtest no subconjunto afetado:
     cargo run --release -p psx-core --example hwtest -- \
       --bios bios/SCPH1001.BIN --tests <ps1-tests> \
       --only <teste> --verbose
5. Não implementar scheduler cycle-accurate nesta passada, EXCETO se a
   instrumentação da Fase 1 provar que a trava do Xenogears é de
   timing de barramento. Timing de DMA/GPU/waitstates é Fase 5.
6. Não tentar bit-exatidão do IDCT do MDEC. Não implementar memory
   card, CD-DA nem XA nesta passada.
7. Medir Getloc pelo TOTAL de casos do ps1-tests/cdrom/getloc, não
   por um caso isolado. Uma correção que troca “falha esperada que
   passava” por “sucesso esperado que falha” é regressão (já aconteceu).


══════════════════════════════════════════════════════════════════
FASE 1 — TRAVA DO XENOGEARS (prioridade absoluta)
══════════════════════════════════════════════════════════════════

Sintoma: o jogo carrega 299 setores e entra no laço infinito
Pause → Setloc → ReadN. Sem exceção, sem acesso inválido, sem fila
cheia. DMA3_CHCR é lido 164 vezes por escrita. O contador
`next_sector_in` em advance_read NÃO chega a zero durante a trava,
embora o drive esteja em Reading.

Hipótese principal (cruzar com o hardware ANTES de “adivinhar”):

A) A máquina de status do CD-ROM está errada.
   PSX-SPX: bits Play[7], Seek[6], Read[5] são MUTUAMENTE EXCLUSIVOS.
   Read/Play SÓ ligam DEPOIS que o seek implícito termina.
   GetlocL durante seek falha com INT5, reason 80h.
   GetlocP funciona durante seek.
   Pause emitido enquanto o drive ainda está em seek / antes do
   primeiro setor: o mecanismo real recusa com INT5(stat|ERROR, 80h)
   (confirmado no DuckStation com teste de hardware). Se o Pause
   “passar” e resetar `next_sector_in` via ReadN seguinte, o setor
   nunca nasce — exatamente a contradição documentada.

B) DMA3 está busy para sempre porque não há DRQ.
   CD-ROM no hardware usa SyncMode=0 (manual + trigger), NÃO
   SyncMode=1. Bit 24 limpa na CONCLUSÃO da transferência; bit 28
   limpa no INÍCIO. Se o canal 3 espera request/DRQ e o CD nunca
   arma BFRD/DRQSTS, o jogo poleia CHCR para sempre.
   Referência: psx-spx DMA Channels; CHCR típico de CD é 0x11000000
   ou 0x11400100.

C) advance_read está engatado.
   Se o corpo de `advance_read` só produz setor quando
   `pending.is_empty() && interrupt_flags == 0 && fifo vazia`, um
   ACK/INT3 não reconhecido (ou um Pause no meio) impede o INT1
   para sempre. Confirme no código atual de
   crates/psx-core/src/cdrom/mod.rs.

Trabalho obrigatório, nesta ordem:

1. Instrumentar o laço do DMA3_CHCR (o doc já pede isso).
   Para cada leitura de 0x1F8010B8 durante a trava, registrar:
   - PC
   - valor devolvido (especialmente bit 24 busy e bit 28 trigger)
   - SyncMode de CHCR
   - fase do drive (Idle/Reading/Seeking)
   - status byte (bits Play/Seek/Read/Motor)
   - sector_available, data_requested (BFRD), DRQSTS
   - interrupt_flags, interrupt_enable, fila pending
   - next_sector_in
   Use iotrace + um log específico. Não chute.

2. Com o log na mão, aplicar SOMENTE as correções que o log exigir.
   Candidatos concretos a verificar no código:
   - ReadN/ReadS: INT3 imediato com Seek=1, Read=0, Motor=1 se o
     Setloc ainda não foi consumido. Quando o primeiro setor
     chega: Seek=0, Read=1, dispara INT1(stat). Nunca Seek e Read
     juntos.
   - Setloc NÃO inicia seek e NÃO interrompe Read em andamento;
     só marca o alvo como unprocessed. ReadN com Setloc
     unprocessed faz seek implícito e marca processed. ReadN sem
     Setloc novo continua a leitura corrente.
   - Pause: INT3(stat atual, Read ainda 1 se estava lendo) depois
     INT2(stat com Read=0). Se o drive está Seeking ou Reading sem
     ter produzido o primeiro setor, responder INT5(ERROR, 80h) e
     NÃO cancelar o seek — comportamento verificado no silício.
   - GetlocL: INT5(80h) se Seeking ou se o header ainda não é
     válido. NÃO inventar flag position_valid no GetlocP.
   - GetlocP: funciona durante seek; devolve SubQ em BCD
     (track, index, rel MSF, abs MSF).
   - Produção de setor NÃO pode depender de interrupt_flags != 0.
     INT1 entra na fila; se já há IRQ pendente, atrasa a entrega
     mas NÃO zera nem congela next_sector_in para sempre.
   - BFRD (bit 7 de 1F801803 index 0): 1 copia staged_sector →
     FIFO e arma DRQSTS. 0 esvazia a FIFO. DMA3 só transfere com
     DRQSTS=1. Sem dados, SyncMode 0 NÃO deve fingir conclusão
     com lixo, nem ficar busy se o hardware no modo manual
     transferiria zeros — decida com o log + SPX, não por feeling.
   - Após INT1, o jogo precisa ack (write 0x1F em HCLRCTL) antes
     do próximo INT1. Se o ack não veio, NÃO sobrescrever o IRQ;
     atrasar o próximo setor.

3. Critério de aceite da Fase 1:
   - O laço Pause→Setloc→ReadN termina.
   - Setores passam de 299.
   - Screenshot após N frames mostra a tela seguinte ao ponto da
     trava (não framebuffer congelado).
   - cdrom/getloc não regride no total de casos.
   - Testes internos do CD-ROM cobrindo:
     * mutex Play/Seek/Read
     * ReadN INT3 com Seek=1 Read=0, depois INT1 com Seek=0 Read=1
     * GetlocL durante seek → INT5 80h
     * GetlocP durante seek → sucesso
     * Pause durante seek implícito pré-primeiro-setor → INT5 80h
     * Setloc não interrompe Read
     * BFRD/DRQSTS/DMA3: busy baixa só após as palavras pedidas
       terem saído da FIFO


══════════════════════════════════════════════════════════════════
FASE 2 — BUGS ISOLADOS DO ps1-tests
══════════════════════════════════════════════════════════════════

### 2.1 cpu/code-in-io

Buscar instrução da scratchpad já dá bus error (is_executable em
memory/mod.rs). Falta a região de I/O inteira, em especial MDEC
(0x1F801820 / 0x1F801824). Hoje o fetch devolve lixo que se
desassembla como salto para si mesmo e o emulador prende.

Ajuste:
- Fetch de instrução e prefetch de I-cache devem passar pela
  MESMA verificação de executabilidade que dados.
- Não é executável: scratchpad 0x1F800000-0x1F8003FF, I/O
  0x1F801000-0x1F801FFF, expansion 2 0x1F802000-0x1F802FFF,
  e o bloco do MDEC.
- Exceção: Bus Error de instrução (AdEL no COP0, EPC = PC
  culpado, Cause.ExcCode = 4). Confirme o código exato contra
  o psx.log do teste.
- Teste unitário: setar PC para 0x1F801820, executar 1
  instrução, assert exception. Não pode loopar.

### 2.2 cpu/io-access-bitwidth

Já está certo: escrita estreita entrega a palavra inteira do
registrador da CPU; o periférico escolhe a largura
(store_io_wide em bus.rs). Sobram dois pontos.

GPUSTAT (0x1F801814), campo de resolução (bits 16-18: HRes2 +
HRes1):
- Leitura de 8 bits em offset 0, 1, 2, 3 e de 16 bits em
  offset 0 e 2 deve devolver o recorte little-endian da palavra
  de 32 bits, sem “zerar” bits que caem fora do lane.
- Implementar load_io_wide simétrico ao store: o periférico
  sempre vê/produz 32 bits; o barramento extrai o lane.
- Teste unitário com os offsets que o ps1-tests exercita.
  Comparar com o psx.log.

Expansion 3 (0x1FA00000, 2 MB):
- No retail não há SRAM. Comportamento típico: open bus
  (devolve o último valor do barramento) ou 0. Bater com o
  psx.log do teste, não com a intuição.
- Mapear a região no bus para NÃO cair no caminho de “lixo
  executável” / crash. Fetch nessa região também é bus error.

### 2.3 cdrom/getloc

Quatro falhas: bits de status em seek/leitura e respostas
GetlocL/GetlocP.

Regras (PSX-SPX “CDROM Status Commands” + testes documentados
no próprio psx-spx):
- GetlocL devolve header+subheader do setor MAIS NOVO no
  buffer: min,sec,frame,mode,file,channel,sm,ci (BCD).
- GetlocL FALHA INT5(80h) durante Seek e em CD-DA.
- GetlocP devolve SubQ: track,index,rmin,rsec,rframe,amin,asec,
  aframe (tudo BCD). Funciona durante Seek. Sem flag caseiro.
- Status no INT3 de Getstat/Read/Pause segue o mutex
  Play/Seek/Read.
- Medir pelo total. Rodar hwtest --only cdrom/getloc --verbose
  e só aceitar se o número de casos passando subir sem criar
  falha nova.

Critério de aceite da Fase 2:
- cpu/code-in-io passa no hwtest.
- cpu/io-access-bitwidth passa no hwtest.
- cdrom/getloc: número de casos passando ≥ estado atual + os 4
  que falhavam, ou o máximo atingível sem regressão. Documentar
  qualquer caso que continue falhando, com a linha do psx.log.


══════════════════════════════════════════════════════════════════
FASE 3 — SPU memory-transfer (bloqueante para áudio e para o teste)
══════════════════════════════════════════════════════════════════

Não implementar mixagem completa das 24 vozes ainda. Implementar
o CAMINHO DE MEMÓRIA que o teste spu/memory-transfer exige.

PSX-SPX Sound Processing Unit:
- Transferência PIO: registradores 0x1F801DA6 (endereço SPU),
  0x1F801DA8 (FIFO de dados), 0x1F801DAA (SPUCNT bits de
  transferência).
- Transferência DMA4: SyncMode 1 (request), direção definida
  por SPUCNT. Transferência em blocos quando o SPU arma DRQ.
- SPU RAM = 512 KiB. Endereço em halfwords. Escritas de 32 bits
  no porto de dados são NÃO confiáveis no hardware; o teste
  cobre isso — 16 bits é o caminho certo.
- Após DMA, bit busy do CHCR desce e IRQ de DMA4 sobe se
  habilitado.

Testes:
- Unitário: write PIO 16-bit em 0x100, read back.
- Unitário: DMA4 RAM→SPU e SPU→RAM de um bloco conhecido.
- hwtest --only spu/memory-transfer deve passar.

Depois que o teste passar, se ainda houver tempo: esqueleto de
mixagem (24 vozes, ADPCM 4-bit com flags de loop, ADSR, pitch)
o bastante para não ser silêncio. Não é bloqueante desta
passada se memory-transfer passar.


══════════════════════════════════════════════════════════════════
FASE 4 — TESTES DE REGRESSÃO OBRIGATÓRIOS
══════════════════════════════════════════════════════════════════

Rodar e colar o resumo no final:

A. Internos
   cargo test --workspace
   cargo clippy --workspace --all-targets -- -D warnings
   cargo fmt --all --check
   Nenhum teste que hoje passa pode falhar.

B. ps1-tests (hwtest --verbose)
   Mínimo a executar:
   - cpu/code-in-io
   - cpu/io-access-bitwidth
   - cpu/cop                (já passava — regressão)
   - cdrom/getloc
   - cdrom/timing           (pode continuar falhando; só não piorar)
   - dma/chopping           (idem)
   - dma/chain-looping      (comportamento observável já batia)
   - gpu/* que já passavam
   - gte/*                  (1150/1150 — NÃO REGREDIR)
   - spu/memory-transfer
   - mdec/8bit              (pode continuar com erro ±1; não piorar)
   Relatar: N passou / N total, e o delta contra 6/21.

C. Xenogears
   cargo run --release -p psx-core --example screenshot -- \
     --bios bios/SCPH1001.BIN \
     --disc games/xenogears/xenogears-disk-1.cue \
     --frames 4000
   Antes: travava ~depois de 299 setores. Depois: frames > 1500
   com framebuffer diferente do frame da trava.
   Confirmar com iotrace que DMA3_CHCR deixou de ser o ranking #1
   de “leituras por escrita” na janela pós-boot.

D. BIOS
   Screenshot da shell Sony após boot sem disco deve continuar
   ≥ 99,6% dos pixels de referência.


══════════════════════════════════════════════════════════════════
FASE 5 — FORA DE ESCOPO (só documentar, não implementar agora)
══════════════════════════════════════════════════════════════════

- Scheduler que intercala DMA e CPU (dma/chopping, gpu/bandwidth,
  cpu/access-time, cdrom/timing, timers via custo de DMA).
- Bit-exatidão do IDCT do MDEC.
- cdrom/disc-swap (precisa de gatilho sintético de tampa).
- Memory card, DualShock analógico, CD-DA, XA.
- Emulador de referência instrumentado (Avocado) — só voltar a
  isso se a Fase 1 não fechar a trava.

Se a Fase 1 falhar DEPOIS da instrumentação e a evidência apontar
para “o jogo espera N ciclos de DMA que nós cobramos 0”, AÍ e só
aí implemente um scheduler mínimo: DMA3/GPU cobram ciclos por
palavra e devolvem a CPU no intervalo. Não construa o scheduler
completo dos 6 testes nesta passada.


══════════════════════════════════════════════════════════════════
ORDEM DE COMMITS (Conventional Commits)
══════════════════════════════════════════════════════════════════

1. test(cdrom): instrumenta DMA3_CHCR + estado do drive na trava
2. fix(cdrom): mutex Seek/Read/Play e respostas INT3/INT1/INT5
3. fix(cdrom): Pause durante seek devolve INT5 80h
4. fix(dma): CHCR do canal 3 — busy, trigger, DRQ, SyncMode 0
5. fix(cpu): fetch em I/O e MDEC gera bus error
6. fix(bus): GPUSTAT e Expansion 3 em 8/16 bits
7. fix(cdrom): GetlocL/GetlocP contra ps1-tests
8. fix(spu): PIO + DMA4 memory-transfer
9. test: unitários + atualiza docs/erros-e-ajustes.md e
   docs/pendencias-hardware.md com o que passou e o que restou

Atualize os dois docs no último commit. Não deixe diagnóstico
stale.


══════════════════════════════════════════════════════════════════
CRITÉRIO DE “EMULAÇÃO CORRETA” DESTA PASSADA
══════════════════════════════════════════════════════════════════

Não prometa “perfeita”. Defina o alvo mensurável:

- 236 testes internos continuam verdes (ou aumentam).
- ps1-tests: code-in-io, io-access-bitwidth, getloc,
  spu/memory-transfer passam. GTE 1150/1150 mantido.
- Xenogears passa do laço Pause/Setloc/ReadN.
- BIOS shell pixel-match ≥ 99,6%.
- Nenhuma regressão clippy/fmt.

Comece pela Fase 1 item 1 (instrumentação). Não aplique o
“fix” do status bit antes de ter o log do DMA3_CHCR cruzado
com o estado do drive no mesmo ciclo.
```

---

Fim do prompt copiável.
