# Onde nós divergimos do console

Duas fontes cruzadas contra a nossa implementação, para achar causa dos bugs
abertos: o [rustation-ng](https://github.com/simias/rustation-ng) (Rust,
GPLv2), que dá comparação de estrutura, e o
[markdown do PSX-SPX](https://github.com/psx-spx/psx-spx.github.io/tree/master/docs),
que dá a regra escrita. Cada item tem o que o console faz, o que nós fazemos,
por que a diferença importa e o que mudar.

Lidos: do rustation, `src/psx/spu.rs`, `dma.rs`, `irq.rs`, `mdec.rs` e
`cd/cdc/decoder.rs`; do PSX-SPX, `cdromdrive.md`, `interrupts.md`,
`dmachannels.md`, `soundprocessingunitspu.md` e `macroblockdecodermdec.md`.

O markdown vale mais que o site renderizado para varredura: dá para procurar o
texto inteiro de uma vez, e é lá que estão as seções que o índice do site
esconde — "First Response", "Responses", "BUSYSTS flag".

---

## Antes de comparar: onde eles estão em outro nível

O CD-ROM deles **não é comparável ao nosso**. Eles emulam o
microcontrolador MC68HC05 do drive rodando a ROM de verdade
(`cd/cdc/uc.rs`, 41 KB), mais o chip decodificador CXD1199
(`decoder.rs`, 51 KB) e o DSP (`dsp.rs`, 53 KB). Os comandos do CD não são
interpretados por eles: são executados pelo firmware original.

Nós interpretamos os comandos direto. Isso é uma escolha de escopo, não um
erro — mas significa que só dá para tirar dali a **semântica dos
registradores**, não a estrutura.

A SPU, o DMA, o MDEC e o controlador de IRQ são implementações diretas como as
nossas, e aí a comparação vale linha a linha.

---

## 1. Interrupções: linha de nível, não pulso

**O achado mais importante desta análise.**

No rustation o controlador de IRQ guarda, além do `I_STAT`, o **nível de cada
linha física**:

```rust
pub fn set_high(psx: &mut Psx, which: Interrupt) {
    if psx.irq.level & m != 0 { return; }   // sem mudança
    psx.irq.level |= m;                     // borda de subida
    psx.irq.status |= m;
}
```

E cada periférico **publica o seu nível continuamente**, deixando a detecção
de borda com o controlador:

```rust
// cd.rs
irq::set_level(psx, Interrupt::CdRom, psx.cd.cdc.irq_active());
// dma.rs
irq::set_level(psx, Interrupt::Dma, irq.is_active());
```

Nós não temos o conceito de nível: o periférico chama `irq.raise(...)` no
instante do evento. Foi por isso que precisei implementar a detecção de borda
do DMA à mão, e errei na primeira vez — o commit `3d826da` desta semana.

### Por que isso é um bug aberto, e não só uma diferença de estilo

O nosso CD-ROM só levanta a IRQ **no momento em que entrega a resposta**:

```rust
self.interrupt_flags = ready.interrupt as u8;
if self.interrupt_enable & self.interrupt_flags != 0 {
    irq.raise(Interrupt::CdRom);
}
```

Se o jogo habilitar a interrupção **depois** de a flag já estar pendente, a
IRQ nunca é levantada. E como `step()` para de entregar respostas enquanto
`interrupt_flags != 0`, o controlador inteiro trava.

Isso não é hipotético: no rastro do Guilty Gear o handler escreve o registrador
de habilitação (`0x07`) **depois** de reconhecer a interrupção, a cada volta.
Uma corrida entre esses dois passos deixa a flag acesa com a IRQ perdida.

**Ação:** dar ao `IrqController` um `set_level(fonte, alto)` com detecção de
borda interna, e fazer o CD-ROM publicar `interrupt_enable & interrupt_flags != 0`
a cada `step`. Vale o mesmo para o DMA, que hoje tem a borda calculada à mão.

---

## 2. CD-ROM: três semânticas de registrador que nos faltam

Do `decoder.rs`, que é o chip que responde em `0x1F801800..1803`.

### `BUSYSTS` — bit 7 do registrador de status

```rust
r |= (self.command_busy as u8) << 7;
```

Fica em 1 do momento em que o comando é escrito até o firmware limpá-lo com
`CLRBUSY`. **Nós nunca ligamos esse bit.** Um jogo que espera o comando ser
aceito antes de mandar o próximo vê "nunca ocupado" e pode atropelar a fila.

### `BFRD` durante transferência é ignorado

```rust
if v.bit(7) {
    if decoder.hxfrc == 0 {   // só começa se a anterior acabou
        decoder.hxfrc = decoder.hxfr;
        ...
    }
}
```

Nosso `set_data_request(0x80)` **sempre** troca o setor e rebobina o cursor,
mesmo com uma transferência em andamento. Um pedido repetido no meio de uma
leitura descarta o resto do setor corrente.

### `DRQSTS` é um contador, não "a FIFO tem bytes"

```rust
r.set_bit(6, self.hxfrc > 0);
```

`hxfrc` é a contagem de palavras que ainda faltam na transferência corrente,
carregada de um registrador de tamanho programável. O nosso é
`data_requested && cursor < len` — aproximação razoável, mas que ignora o
tamanho programado.

**Ação:** ligar `BUSYSTS`, e condicionar a recarga do setor em `BFRD` a não
haver transferência em curso. O contador programável fica para depois.

---

## 3. SPU: as vozes nunca param

O comentário deles é direto:

> *"There's no 'enable' flag for the voices, they're effectively always
> running. Unused voices are just muted. Beyond that the ADPCM decoder is
> always running, even when the voice is in 'noise' mode and the output isn't
> used. **This is important when the SPU interrupt is enabled.**"*

A nossa `Voice` tem `Phase::Off`, e `sample()` sai cedo nesse estado:

```rust
if !self.is_on() {
    self.last_sample = 0;
    return 0;
}
```

Com isso o decodificador ADPCM para, o endereço corrente congela e a **IRQ de
endereço da SPU nunca dispara** para aquela voz. Um jogo que usa essa IRQ para
saber que um bloco de sample foi consumido espera para sempre.

**Ação:** rodar o decodificador sempre e usar o envelope apenas para silenciar
a saída. `Phase::Off` deixa de parar a voz e passa a ser só nível zero.

---

## 4. SPU: o decaimento exponencial pode empacar no nosso

Eles:

```rust
EnvelopeMode::Exponential => ((ls * cl) >> 15) as i16
```

Nós:

```rust
step = (step * i32::from(level)) / 0x8000;
```

Para valores **negativos** — que é justamente o caso do decay e do release —
`>> 15` e `/ 0x8000` não são a mesma coisa: o deslocamento arredonda para
baixo (`-800 >> 15 == -1`) e a divisão trunca para zero (`-800 / 32768 == 0`).

Com passo zero o nível para de descer e o release nunca chega ao fim. A voz
fica presa soando baixo em vez de calar.

**Ação:** trocar a divisão por deslocamento aritmético. É uma linha, e o teste
que a cobre é um release lento chegando a zero.

---

## 5. SPU: o divisor do envelope pode pular passos

Eles acumulam e comparam com o limiar:

```rust
self.divider += div_step;
if self.divider < 0x8000 { return; }
self.divider = 0;
```

Nós testamos o bit:

```rust
self.adsr_counter = self.adsr_counter.wrapping_add(increment);
if self.adsr_counter & 0x8000 == 0 { return; }
```

Como o nosso contador é `u32` e só zera quando o bit 15 aparece, um acúmulo
que passe de `0x10000` volta a ter o bit 15 em zero e o passo é **pulado**.
O deles não tem esse buraco porque compara com o limiar.

**Ação:** comparar com `0x8000` em vez de testar o bit.

---

## 6. SPU: o mute não silencia o áudio de CD

```rust
if psx.spu.muted() {
    // Mute bit doesn't actually mute CD audio, just the SPU voices.
    left_mix = 0;
    right_mix = 0;
}
let [cd_left, cd_right] = cd::run_audio_cycle(psx);   // depois do mute
```

Nós zeramos tudo:

```rust
if control & (ENABLE | UNMUTE) != (ENABLE | UNMUTE) {
    return (0, 0);
}
```

Silenciar a SPU enquanto uma trilha XA toca é padrão em tela de carregamento.
Do nosso jeito o áudio some.

**Ação:** aplicar o mute só à soma das vozes, somando o CD depois.

---

## 7. SPU: os buffers de captura não existem no nosso

```rust
ram_write(psx, psx.spu.capture_index, cd_left as u16);          // 0x000
ram_write(psx, psx.spu.capture_index | 0x200, cd_right as u16); // 0x200
// e, dentro do laço das vozes:
if voice == 1 { ram_write(psx, 0x400 | capture_index, sample); }
if voice == 3 { ram_write(psx, 0x600 | capture_index, sample); }
```

O primeiro kilobyte da SPU RAM é escrito continuamente com o áudio de CD e com
a saída das vozes 1 e 3, num índice que dá a volta a cada 512 amostras. Jogos
leem esses buffers para medir volume e sincronizar. Nós não escrevemos nada
ali.

**Ação:** implementar a captura. É barato e pode ser exatamente o que um jogo
espera ver mudar.

---

## 8. DMA: prontidão consultada por dispositivo

```rust
fn can_run(psx: &mut Psx, port: Port, write: bool) -> bool {
    if write {
        match port {
            Port::Gpu => gpu::dma_can_write(psx),
            Port::MDecIn => mdec::dma_can_write(psx),
            ...
```

O canal só anda quando o periférico diz que aceita. Nosso DMA é atômico e roda
inteiro dentro da escrita do `CHCR`, sem perguntar nada — foi assim que o canal
do MDEC passou do buffer do jogo e apagou o vetor de exceção (`d7b9cff`).

Vale registrar que o comentário deles sobre o canal do CD-ROM é uma dúvida
aberta: *"Does this make sense? Can the CDC block the DMA if no sector has been
read?"* — eles devolvem `true`. Ou seja, a pergunta que eu levantei nesta
sessão também não tem resposta fechada lá.

**Ação:** nenhuma imediata. É o scheduler de ciclos, que segue fora do MVP.

---

## 9. O que nem eles implementam

Útil para calibrar escopo:

- **Sweep de volume**: `VolumeConfig::Sweep(_) => unimplemented!()` — o
  rustation entra em pânico. A nossa aproximação por nível fixo é pior em
  precisão e melhor em robustez; fica como está, documentada.
- **`BFWR`** (escrita para o buffer do decodificador) e o modo *sound map*:
  `unimplemented!()`.

---

## 10. Comandos são retidos enquanto houver interrupção pendente

PSX-SPX, "First Response (INT3)", sobre o mainloop do drive:

> *"Main CPU has sent a command, **AND, there is no INT pending** (if an INT is
> pending, then the command won't be executed yet, but will be executed in
> following mainloop cycles; once when INT got acknowledged)."*

Nós executávamos o comando dentro da própria escrita do registrador. O
hardware o **retém** até o software reconhecer a interrupção anterior.

Isso também dá o `BUSYSTS` da seção 2 de graça: o bit 7 do status é exatamente
"há comando escrito e ainda não aceito".

**Feito.** Um teste cobre o caso: com a interrupção acesa, o comando seguinte
não responde; depois do acknowledge, responde.

## 11. A fila de respostas não é uma fila

Mesma página, seção "Responses":

> *"Instead of using a real queue, it's merely using some flags (...) There is
> no flag for First Response (INT3); because that INT is generated immediately
> after executing a command. The flag mechanism means that the SUB-CPU cannot
> hold more than one undelivered INT1."*

Ou seja: **uma** flag de INT2, **uma** de INT1, e o INT3 sai na hora da
execução. Nosso `pending` é um `VecDeque` sem limite.

Na prática a retenção de comando da seção 10 já impede a fila de crescer
muito, mas o modelo continua diferente do silício. Um jogo que conte com
perder um INT1 antigo vê comportamento diferente aqui.

**Ação:** trocar a fila por dois campos, `second_response` e `data_response`.
Não é urgente e mexe em código sensível.

## 12. Um comando novo pode cancelar a segunda resposta do anterior

Mesma página, seção "BUSYSTS flag":

> *"`Stop -> Wait for INT3 IRQ -> clear IRQ -> SetMode/Pause/...` Will **drop
> the second response of Stop()**, and then execute the next command."*

Para `Pause`, `ReadN` e `ReadS` o texto diz que nada é descartado. Ou seja: é
por comando, não uma regra geral — e só o `Stop` está documentado.

Nós nunca descartamos. Grandstream e Xenogears usam `Pause` seguido de
`Setloc`, que segundo o texto **não** descarta; então isto provavelmente não os
afeta.

**Ação:** nenhuma. Fica registrado para não ser redescoberto como novidade.

---

## Ordem de ataque

Já aplicados, todos verificados contra os portões e **nenhum deles destravou os
três jogos**: IRQ por nível (1), `BUSYSTS` e retenção de comando (2 e 10),
vozes sempre rodando (3), decaimento exponencial e divisor do envelope (4 e 5),
mute do CD e captura (6 e 7).

O que resta, em ordem:

1. **`BFRD` durante transferência em curso** (seção 2). O único item da lista
   com efeito possível sobre o fluxo de setores: hoje um pedido repetido no
   meio de uma leitura descarta o resto do setor corrente, e os três jogos
   travados escrevem nesse registrador a cada INT1.
2. **Duas flags em vez de fila de respostas** (seção 11). Aproxima o modelo do
   silício; mexe em código sensível, então só com sintoma que a justifique.
3. **Prontidão de DMA por dispositivo** (seção 8), que é o scheduler de ciclos
   e segue fora do MVP.

---

## Licenca e procedencia

O rustation-ng e GPLv2. Este documento descreve comportamento observado para
guiar implementacao independente; **nenhum codigo foi copiado**, e nenhum deve
ser. Os trechos citados sao referencia de leitura, no volume minimo para
identificar a diferenca. O PSX-SPX e documentacao, e as citacoes dele estao
marcadas como tal.
