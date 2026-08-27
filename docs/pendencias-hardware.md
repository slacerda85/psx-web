# Pendências verificadas contra o hardware

Estado da comparação com o [ps1-tests](https://github.com/JaCzekanski/ps1-tests),
cujo `psx.log` ao lado de cada `.exe` é a saída do **PlayStation real**.

Reproduzir:

```sh
cargo run --release -p psx-core --example hwtest -- \
    --bios bios/SCPH1001.BIN --tests caminho/para/ps1-tests --verbose
```

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
| `cpu/access-time` | Waitstates por região do mapa de memória |
| `cdrom/timing` | Latência de resposta dos comandos do drive |
| `timers` / `timer-dump` | Os contadores já batem exatamente; o que falha é o custo do DMA aparecer como zero |

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

## Bugs a investigar

Estes não têm causa conhecida ainda — cada um precisa do mesmo tratamento que
levou às correções de GTE, SIO e DMA: rodar, ler a diferença, isolar.

| Teste | Sintoma |
| --- | --- |
| `cpu/code-in-io` | Execução de código a partir da scratchpad e de portas de I/O |
| `cpu/io-access-bitwidth` | Larguras de acesso (8/16/32 bits) por registrador; divergimos em `DMA0_ADDR`, `DMAC_CTRL` e `JOY_MODE` |
| `cdrom/disc-swap` | Comportamento ao abrir/fechar a bandeja |
| `cdrom/getloc` | Bits de status durante seek e leitura, e as respostas de `GetlocL`/`GetlocP` |

## Trava

| Teste | Sintoma |
| --- | --- |
| `dma/chain-looping` | Lista encadeada auto-referente. O teste estoura o tempo limite: provavelmente seguimos o laço para sempre onde o hardware sai. É bug nosso, não limitação. |

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
