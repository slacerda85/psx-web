# Pendências verificadas contra o hardware

Estado da comparação com o [ps1-tests](https://github.com/JaCzekanski/ps1-tests),
cujo `psx.log` ao lado de cada `.exe` é a saída do **PlayStation real**.

Reproduzir:

```sh
cargo run --release -p psx-core --example hwtest -- \
    --bios bios/SCPH1001.BIN --tests caminho/para/ps1-tests --verbose
```

O diagnóstico detalhado de cada falha, com a evidência que a sustenta e o
próximo passo, está em [erros-e-ajustes.md](erros-e-ajustes.md).

**6 de 21 batendo com o hardware.** O que falta está abaixo, agrupado pelo tipo
de trabalho que cada um exige.

## Timing de barramento

Nosso DMA é atômico: a transferência inteira acontece dentro da escrita que a
dispara, sem cobrar ciclos da CPU. A GPU também não cobra tempo de desenho.
Esses testes medem exatamente isso, então todos falham pela mesma causa e
serão resolvidos juntos, quando houver um scheduler que intercale DMA e CPU.

| Teste | O que mede |
| --- | --- |
| `dma/chopping` | Ciclos que o DMA rouba da CPU por bloco (reportamos 0) |
| `gpu/bandwidth` | Vazão de transferências para a VRAM |
| `cdrom/timing` | Latência de resposta dos comandos do drive |
| `timers` / `timer-dump` | Os contadores já batem exatamente; o que falha é o custo do DMA aparecer como zero |
| `dma/chain-looping` | Cadeia auto-referente: o comportamento observável (não conclui, sem IRQ) já bate; sobra o custo em ciclos |

## Subsistemas ausentes

| Teste | Falta |
| --- | --- |
| `mdec/4bit`, `mdec/8bit`, `mdec/step-by-step-log` | O MDEC existe e decodifica; ver abaixo |
| `spu/memory-transfer` | Caminho de transferência da SPU RAM |

## MDEC — decodifica e transporta, com arredondamento residual

O decodificador e o DMA de saída funcionam. Comparado ao console em 8 bpp, a
maioria dos bytes bate exatamente e alguns divergem em 1 ou 2 — arredondamento
dentro do IDCT, não erro estrutural. Bom o bastante para FMV; ficar bit a bit
igual exige achar o arredondamento exato que o silício usa nos deslocamentos
internos.

## Tempo de acesso à memória — resolvido

O `cpu/access-time` agora fica dentro de um ciclo do console em todas as
regiões. Ele continua marcado como diferente porque o log de referência traz
médias com jitter — o console mede 5,21, 5,3 e 5,14 ciclos para o mesmo acesso
à RAM, e igualdade textual é impossível por construção.

Não era refinamento: com um ciclo por acesso, a `VSync()` da biblioteca da
Sony estourava o prazo em todo quadro e nenhum jogo passava do boot.

## Bugs a investigar

Estes não têm causa conhecida ainda — cada um precisa do mesmo tratamento que
levou às correções de GTE, SIO e DMA: rodar, ler a diferença, isolar.

| Teste | Sintoma |
| --- | --- |
| `cpu/code-in-io` | A scratchpad já dá bus error; buscar instrução da região do MDEC prende o emulador num laço |
| `cpu/io-access-bitwidth` | Restam o campo de resolução em `GPUSTAT` e a região de Expansion 3 |
| `cdrom/disc-swap` | Espera um humano abrir a tampa — não automatizável sem um gatilho sintético no harness |
| `cdrom/getloc` | Bits de status durante seek e leitura, e as respostas de `GetlocL`/`GetlocP` |

## Onde os jogos param

Os dois discos de teste passam pelo BOOTSTRAP LOADER, carregam o executável,
executam código próprio e inicializam o driver de controle. Nenhum desenha na
tela ainda: os dois param no streaming de XA, sem nunca chegar a usar o MDEC.

O Xenogears monta o display de 24 bits do FMV de abertura e para de recolher
setores — o drive entrega quase 7000, ele recolhe 365. O Grandstream Saga nem
liga o display. O diagnóstico completo dos dois, com o que já foi descartado,
está em [erros-e-ajustes.md](erros-e-ajustes.md).

## Já corrigidos por esta comparação

Ficam registrados porque cada um é uma classe de erro que volta fácil:

- **GTE** — registradores de 16 bits tratados como 32; saturação a partir do
  MAC truncado; overflow checado em bloco em vez de por estágio do
  multiplicador-acumulador. De 0 para 1150/1150.
- **CPU** — COP1/COP3 habilitados devem virar no-op, não exceção; `LWC2`/`SWC2`
  não checavam `SR.CU2`.
- **GPU** — bit 15 do GPUSTAT sem checar a permissão do `GP1(0x09)`;
  transferência CPU→VRAM ignorando a máscara do `GP0(0xE6)`.
- **DMA** — o canal 6 é cabeado e ficava inerte fora do modo manual.
- **SIO0** — o `/ACK` do controller é atrasado, não simultâneo ao byte.
- **Timers** — avançavam em bloco no fim da scanline; o dot clock usava
  divisor inteiro onde a razão é 11/7.
