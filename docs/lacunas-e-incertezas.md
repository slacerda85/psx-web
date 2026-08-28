# Lacunas técnicas e pontos sem confirmação

Tudo o que está implementado por aproximação, por dedução ou sem referência
que o sustente. A regra que este arquivo serve: **não inferir nada em
silêncio**. Se um comportamento foi escolhido sem base documental ou medição,
ele aparece aqui, com o que falta para fechá-lo.

Cada item diz o que fizemos, em que nos baseamos (ou não), e o que resolveria a
dúvida. Última revisão: 28/08/2026.

---

## 1. CD-ROM

### 1.1 O setor não entregue é perdido ou segurado?

**Estado:** seguramos. Enquanto a INT1 anterior não é reconhecida, o setor fica
no buffer e é entregue depois.

**O que a fonte diz:** PSX-SPX, "Sector Buffer", afirma o contrário — *"sectors
would be lost without notice (there appear to be absolutely no overrun status
flags, nor overrun error interrupts)"*. O buffer tem 8 slots dos quais só 2 são
alcançáveis: *"the oldest sector, and the current/newest sector"*, e depois de
processar o mais antigo o controlador **pula para o mais novo**.

**Por que não seguimos a fonte:** perder setores exige modelar os dois slots e
o salto, e a versão que segura é mais tolerante. Trocar às cegas pode piorar.

**Como fechar:** implementar os dois slots com o salto para o mais novo e medir
os quatro jogos antes e depois. O `cdrom/getloc` do ps1-tests é o teste que
mais chega perto disso.

### 1.2 O tamanho da transferência do decodificador é programável

**Estado:** `DRQSTS` é `data_requested && cursor < len`.

**O que a fonte diz:** no chip real (visto no `decoder.rs` do rustation-ng) é
`hxfrc > 0`, um contador carregado de um registrador `HXFR` de tamanho
programável. Não achamos onde o software escreve `HXFR` no PSX.

**Como fechar:** achar no PSX-SPX ou no dump do firmware quem programa `HXFR`.
Enquanto isso, a aproximação vale porque o BIOS sempre transfere um setor
inteiro.

### 1.3 `BFRD` durante transferência em curso

**Estado:** um novo `BFRD` sempre troca o setor e rebobina o cursor.

**O que a fonte diz:** o decodificador só inicia se a transferência anterior
acabou (`if hxfrc == 0`). Sem o contador do item 1.2 não dá para saber quando
"acabou".

**Como fechar:** depende de 1.2.

### 1.4 Quais comandos descartam a segunda resposta

**Estado:** nenhum. A INT2 pendente sempre é entregue.

**O que a fonte diz:** PSX-SPX, "BUSYSTS flag", documenta **só o `Stop`**:
*"Will drop the second response of Stop(), and then execute the next
command"*. Para `Pause`, `ReadN` e `ReadS` diz que nada é descartado. Os
demais comandos com duas respostas — `MotorOn`, `Init`, `SetSession`, `SeekL`,
`SeekP`, `GetID`, `GetQ`, `ReadTOC` — não são mencionados.

**Como fechar:** teste em console real, ou uma fonte que cubra os outros
comandos. Não implementamos o caso do `Stop` porque nenhum jogo de teste o usa
nessa sequência, e implementar só ele seria uma regra pela metade.

### 1.5 Latências dos comandos

**Estado:** médias fixas, medidas pelo `cdrom/timing` do ps1-tests num console
real (`ACKNOWLEDGE_DELAY = 50_000`, `PAUSE_COMPLETE_DELAY = 1_010_000`, etc.).

**A dúvida:** o drive é mecânico e o mesmo comando varia de 24 mil a 180 mil
ciclos no console. Usamos a média. Um jogo que sequencie comandos pelo tempo de
resposta pode depender da variação, não da média.

**Como fechar:** o `cdrom/timing` continua falhando; fazê-lo passar exige
modelar a variação, o que por sua vez exige saber de onde ela vem.

### 1.6 `SEEK_DELAY` não depende da distância

**Estado:** valor fixo de 200 mil ciclos para qualquer seek.

**A dúvida:** no console o tempo depende de quanto a cabeça precisa andar.
Não temos modelo da mecânica, e inventar uma proporção seria fingir precisão.

---

## 2. SPU

### 2.1 Varredura de volume (*sweep*)

**Estado:** aproximada por um nível fixo de meia escala.

**O que sabemos:** o formato do registrador está documentado (bit 15 liga a
varredura, com modo, direção, fase, deslocamento e passo). O que não fizemos
foi a rampa.

**Referência de escopo:** o rustation-ng também não implementa — ele chama
`unimplemented!()` e entra em pânico. Preferimos um valor audível a um
travamento, mas o nível está errado.

**Como fechar:** a rampa usa a mesma máquina do envelope ADSR, que já existe.
É trabalho conhecido, não pesquisa.

### 2.2 Interpolação linear em vez de gaussiana

**Estado:** interpolação linear entre duas amostras.

**O que o console faz:** janela gaussiana de quatro pontos, com uma tabela de
512 entradas documentada no PSX-SPX.

**Impacto:** timbre, não comportamento. Nenhum jogo depende disso para
funcionar.

### 2.3 Reverb

**Estado:** não implementado. Os registradores aceitam escrita e são ignorados.

**Impacto:** efeito ausente. O PSX-SPX documenta o algoritmo por inteiro, então
é implementação, não pesquisa.

### 2.4 Ruído: a fórmula do período — FECHADO

**Estado:** `period = (0x8000 >> shift).max(1) * (4 + step)`.

**A dúvida:** deduzimos a fórmula do formato do registrador. O PSX-SPX descreve
o gerador de ruído, mas não confirmamos esta expressão contra medição nem
contra outra implementação.

**Como fechar:** comparar com o `spu` do ps1-tests, se houver caso que o
exercite.

### 2.5 O que a IRQ de endereço compara — FECHADO

**Estado:** comparamos o endereço do bloco corrente de cada voz com o
registrador de IRQ, mascarado para o bloco de 8 bytes.

**A dúvida:** não confirmamos se o console compara o endereço do bloco ou o
endereço da amostra corrente, nem se a comparação acontece também durante a
transferência por DMA para a SPU RAM. O rustation faz a checagem dentro de
`ram_read`/`ram_write`, o que sugere que **qualquer** acesso à SPU RAM dispara
— inclusive os do software.

**Como fechar:** mover a checagem para os acessos à RAM e medir. É a hipótese
mais provável e não foi testada.

### 2.6 Quantos blocos o decodificador lê à frente — FECHADO

**Estado:** decodificamos um bloco por vez, quando o anterior acaba.

**O que sabemos:** o rustation decodifica 11 amostras à frente, com o
comentário de que o valor vem do Mednafen e de que *"apparently the original
hardware decodes ahead"*. Ninguém sabe o número certo.

**Impacto:** afeta o instante exato em que a IRQ de endereço dispara.

---

## 3. DMA

### 3.1 A transferência é atômica

**Estado:** a transferência inteira acontece dentro da escrita que a dispara,
sem cobrar ciclos da CPU.

**Consequência conhecida:** seis testes do ps1-tests falham por isso
(`dma/chopping`, `gpu/bandwidth`, `cpu/access-time` na parte de DMA,
`cdrom/timing`, `timers`, `timer-dump`).

**A dúvida que fica:** já aconteceu uma vez de uma lacuna de timing ser a
diferença entre "roda" e "não roda" — foi o tempo de acesso à memória. Não dá
para afirmar que esta não é.

### 3.2 O canal do CD-ROM pode ser bloqueado pelo dispositivo?

**Estado:** não bloqueamos; o canal transfere o que houver na FIFO.

**O que sabemos:** o rustation deixa a mesma pergunta escrita no código —
*"Does this make sense? Can the CDC block the DMA if no sector has been
read?"* — e devolve `true`, ou seja, também não bloqueia. Não achamos fonte
que resolva.

---

## 4. MDEC

### 4.1 Arredondamento do IDCT

**Estado:** a maioria dos bytes bate com o console; alguns divergem em 1 ou 2.

**A dúvida:** não sabemos o arredondamento exato que o silício usa nos
deslocamentos internos.

**Como fechar:** o `mdec/step-by-step-log` dá a saída do console bloco a bloco;
é questão de tentativa e comparação, não de pesquisa.

### 4.2 O campo "bloco corrente" do status é fixo

**Estado:** reportamos sempre 4.

**A dúvida:** o console varia entre 0 e 5 conforme o estágio do pipeline
interno, que não modelamos. O valor 4 foi escolhido por ser o que aparece nas
leituras do teste de hardware — mas é um valor observado, não derivado.

---

## 5. GPU

### 5.1 Não há custo de desenho

**Estado:** o rasterizador termina cada comando instantaneamente, e os bits de
prontidão do `GPUSTAT` são sempre "pronto".

**Consequência:** `gpu/bandwidth` falha. Um jogo que meça vazão vê um console
infinitamente rápido.

### 5.2 Bit 14 do `GPUSTAT`

**Estado:** sempre zero, com o comentário de que "não é usado por software
comercial".

**A dúvida:** isso é uma afirmação que herdamos, não algo que medimos.

---

## 6. O que está aberto sem explicação

O item mais importante deste arquivo, porque não é uma aproximação consciente
— é algo que não sabemos.

**Guilty Gear, Grandstream Saga e Xenogears param de recolher setores do CD.**
O drive continua entregando; o jogo reconhece as interrupções, lê a resposta e
não pede os dados.

O que já foi medido e **descartado** como causa, cada um testado
isoladamente e revertido quando não mudou nada:

| Hipótese | Como foi testada | Resultado |
| --- | --- | --- |
| `ADPBUSY` sempre aceso | forçado a zero | sem mudança |
| Classificação de setor XA | roteamento desligado por inteiro | sem mudança |
| Metade alta do `DICR` sem mapeamento | corrigida (era barramento flutuante) | sem mudança |
| IRQ pulsada em vez de nível | controlador passou a detectar borda | sem mudança |
| Comando executado na escrita | passou a ser retido até o acknowledge | sem mudança |
| Cadência do setor presa ao acknowledge | passou a ser mecânica | sem mudança |
| FIFO de resposta não drenada no acknowledge | passou a drenar | sem mudança |
| Vozes paradas travando a IRQ da SPU | decodificador passou a rodar sempre | sem mudança |

Todas as correções acima são certas por si — cada uma tem fonte que a
sustenta. Nenhuma é a causa.

**O que se sabe do sintoma, do Guilty Gear, que é o mais legível:** o driver do
jogo assume o handler de IRQ no frame ~1000 (o `enable` cai de `0x1F` para
`0x07`) e a partir daí recolhe 782 setores e para. Quando ele decide buscar —
19 vezes em 1500 frames — o caminho funciona: ele lê os 8 bytes de cabeçalho do
setor e o conteúdo confere. O laço principal, que é quem pede os dados, é
gated por algo que não identificamos.

**Onde procurar a seguir:** o laço principal em `0x80066Dxx` espera o
`DMA1_CHCR`, o canal de saída do MDEC. O jogo carrega as tabelas de
quantização e IDCT e nunca envia um comando de decodificação, porque não tem
dados — e não tem dados porque não os pede. É circular, e o ponto de entrada
dessa volta ainda não foi achado.

---

## Como manter este arquivo

Um item entra aqui quando a alternativa seria escolher um comportamento sem
base. Um item sai quando há fonte, medição contra o console, ou um teste do
ps1-tests que o cubra — e aí a correção vai para o código com a referência no
comentário.

---

## Fechados nesta passada

### §2.4 — Ruído

Substituído pelo algoritmo do PSX-SPX, "SPU Noise Generator": contador com
sinal andando para baixo, paridade com o `xor 1` que impede o ponto fixo em
zero, e recarga dupla porque com deslocamento grande o período fica menor que
o passo. Testes: sequência determinística do LFSR e a recarga dupla.

### §2.5 e §2.6 — IRQ de endereço da SPU

Todo acesso à SPU RAM passa por `ram_read`/`ram_write`, e a checagem mora
lá. Fonte: *"all voices are permanently reading data from SPU RAM (...) so even
inaudible voices can trigger IRQs"* e *"Setting the IRQ address to
0000h..01FFh will trigger IRQs on writes to the four capture buffers"*.

A §2.6 some junto: a IRQ é na busca do bloco, então quantas amostras o
decodificador lê à frente deixa de importar. Testes: voz lendo o bloco
vigiado, captura, transferência manual e SPU desligada.
