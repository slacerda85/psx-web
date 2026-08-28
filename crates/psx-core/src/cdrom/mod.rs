//! Controlador de CD-ROM.
//!
//! Referência: PSX-SPX — "CDROM Drive", "CDROM Controller I/O Ports",
//! "CDROM Commands", "CDROM Response Timings".
//!
//! **Escopo atual:** o caminho de dados está completo — registradores, FIFOs,
//! IRQs com acknowledge em duas etapas, seek, leitura contínua de setores em
//! velocidade simples ou dupla, entrega por FIFO ou por DMA, e a TOC.
//!
//! **Ainda não implementado:** CD-DA (faixas de áudio tocadas pelo drive),
//! XA-ADPCM e os filtros de setor associados. Comandos desses grupos são
//! contados em `unimplemented_commands` e respondidos com INT5.

pub mod disc;

use std::collections::VecDeque;

pub use disc::{Disc, DiscError, DiscRegion, Msf, Track, TrackKind, SECTOR_RAW, SECTOR_USER};

use crate::irq::{Interrupt, IrqController};
use crate::spu::adpcm::{self, XaCoding};
use crate::CPU_CLOCK_HZ;

/// Códigos de interrupção que o controlador entrega em `0x1F80_1803.Index1`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum CdInterrupt {
    /// Um setor de dados ficou pronto.
    DataReady = 1,
    /// Segunda resposta (fim de comando demorado).
    Complete = 2,
    /// Primeira resposta (acknowledge do comando).
    Acknowledge = 3,
    /// Fim de disco / fim de faixa.
    DataEnd = 4,
    /// Erro.
    Error = 5,
}

/// Bits do byte de status devolvido por `GetStat`.
pub mod status {
    /// Erro no último comando.
    pub const ERROR: u8 = 1 << 0;
    /// O motor está girando.
    pub const MOTOR_ON: u8 = 1 << 1;
    /// Erro de seek.
    pub const SEEK_ERROR: u8 = 1 << 2;
    /// A bandeja está aberta.
    pub const SHELL_OPEN: u8 = 1 << 4;
    /// Lendo setores de dados.
    pub const READING: u8 = 1 << 5;
    /// Posicionando a cabeça.
    pub const SEEKING: u8 = 1 << 6;
    /// Tocando CD-DA.
    pub const PLAYING: u8 = 1 << 7;
}

/// Bits do registrador de modo, escrito por `Setmode`.
mod mode {
    /// Só entrega setores cujo arquivo e canal casam com o `Setfilter`.
    pub const XA_FILTER: u8 = 1 << 3;
    /// Entrega o setor inteiro (2340 B) em vez dos 2048 B de usuário.
    pub const WHOLE_SECTOR: u8 = 1 << 5;
    /// Manda os setores de áudio XA para o SPU em vez de para o CPU.
    pub const XA_ADPCM: u8 = 1 << 6;
    /// Velocidade dupla (2x).
    pub const DOUBLE_SPEED: u8 = 1 << 7;
}

/// Bits do submodo, no terceiro byte do subheader de um setor Mode 2.
mod submode {
    /// O setor carrega áudio ADPCM.
    pub const AUDIO: u8 = 1 << 2;
    /// Tempo real: o drive entrega no compasso, sem reler em caso de erro.
    pub const REAL_TIME: u8 = 1 << 6;
}

/// Para onde vai um setor lido do disco.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Delivery {
    /// Entregue ao CPU com uma INT1.
    Data,
    /// Encaminhado ao decodificador de áudio XA.
    Adpcm,
    /// Ninguém o quer: o drive o descarta em silêncio.
    Discard,
}

/// Uma resposta agendada, entregue depois de `delay` ciclos.
#[derive(Debug, Clone)]
struct PendingResponse {
    delay: i64,
    interrupt: CdInterrupt,
    bytes: Vec<u8>,
}

/// O que o drive está fazendo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Drive {
    Idle,
    /// Lendo setores em sequência a partir de `read_lba`.
    Reading,
}

/// O controlador de CD-ROM.
#[derive(Debug, Clone)]
pub struct CdRom {
    /// Bits 0..1 de `0x1F80_1800`, que selecionam o significado das portas.
    index: u8,
    parameters: VecDeque<u8>,
    response: VecDeque<u8>,
    /// Máscara de interrupções habilitadas (`0x1F80_1802.Index1`).
    interrupt_enable: u8,
    /// Interrupção pendente (`0x1F80_1803.Index1`), 0 = nenhuma.
    interrupt_flags: u8,
    pending: VecDeque<PendingResponse>,
    /// Comando escrito pelo CPU e ainda não entregue ao controlador.
    ///
    /// PSX-SPX, "First Response": o mainloop do drive só executa um comando
    /// quando **não há interrupção pendente**. Escrever o comando não o
    /// executa; ele fica retido até o software reconhecer a interrupção
    /// anterior. Os parâmetros vão junto, tirados da FIFO no momento da
    /// escrita.
    command_pending: Option<(u8, Vec<u8>)>,
    /// Status corrente reportado por `GetStat`.
    status: u8,
    /// Modo corrente, escrito por `Setmode`.
    mode: u8,

    disc: Option<Disc>,

    /// Áudio XA decodificado à espera da SPU, e a taxa do fluxo.
    adpcm_output: Vec<(i16, i16)>,
    adpcm_rate: u32,
    adpcm_history: [adpcm::History; 2],
    /// O decodificador de áudio ainda tem material para tocar (`ADPBUSY`).
    adpcm_busy: bool,
    /// Arquivo e canal selecionados por `Setfilter`, para os setores XA.
    filter_file: u8,
    filter_channel: u8,
    /// Alvo do próximo seek, escrito por `Setloc`.
    seek_target: Msf,
    /// Posição corrente da cabeça.
    position: Msf,
    /// Próximo setor a ser lido.
    read_lba: u32,
    drive: Drive,
    /// Ciclos que faltam para o próximo setor ficar pronto.
    next_sector_in: i64,
    /// A entrega do setor corrente já falhou uma vez por INT pendente.
    delivery_retry: bool,
    /// O prazo do setor venceu e ele ainda não foi entregue.
    ///
    /// Separado do contador de ciclos porque a cadência do drive é mecânica:
    /// o próximo prazo é agendado quando o anterior vence, e não quando o
    /// software finalmente reconhece a interrupção.
    sector_due: bool,

    /// Setor entregue ao CPU ou ao DMA.
    sector_buffer: Vec<u8>,
    sector_cursor: usize,
    /// BFRD (`0x1F80_1803.Index0` bit 7): o CPU pediu os dados do setor.
    data_requested: bool,
    /// Último setor lido, à espera de o CPU pedir os bytes.
    staged_sector: Vec<u8>,
    /// Há um setor novo em `staged_sector` ainda não movido para a FIFO.
    sector_available: bool,

    /// Últimos comandos recebidos, para diagnóstico.
    history: VecDeque<u8>,
    /// Já houve leitura de setor desde o último reset ou seek.
    ///
    /// `GetlocL` devolve o header do último setor lido; sem nenhum, o
    /// hardware responde INT5 em vez de inventar uma posição.
    header_valid: bool,

    /// Setores entregues desde o boot, para diagnóstico.
    sectors_delivered: u64,
    /// Setores que o CPU chegou a recolher, dos que foram entregues.
    ///
    /// A diferença entre os dois separa "o drive não entrega" de "o jogo
    /// recusa o que recebeu" — as duas travas se parecem de fora.
    sectors_collected: u64,
    /// Comandos recebidos sem implementação, para diagnóstico.
    unimplemented: u64,
    last_unimplemented: u8,
}

// Latências medidas num console real pelo `cdrom/timing` do ps1-tests.
//
// O drive é mecânico e as respostas variam bastante (o mesmo comando vai de
// 24 mil a 180 mil ciclos), então usamos a média de cada uma. Os valores que
// tínhamos antes eram de duas a dez vezes curtos demais, e um jogo que
// sequencia comandos pelo tempo de resposta sai do compasso com isso.

/// Acknowledge (INT3) de um comando comum.
const ACKNOWLEDGE_DELAY: i64 = 50_000;

/// `Pause` responde o acknowledge mais rápido que os demais...
const PAUSE_ACK_DELAY: i64 = 28_600;

/// ...e leva perto de um milhão de ciclos para concluir de fato.
const PAUSE_COMPLETE_DELAY: i64 = 1_010_000;

/// `Init`: acknowledge e conclusão.
const INIT_ACK_DELAY: i64 = 75_000;
const INIT_COMPLETE_DELAY: i64 = 476_000;

/// Latência até o primeiro setor depois de `ReadN`, **além** do período.
///
/// O drive precisa alcançar a posição e sincronizar antes de entregar o
/// primeiro setor; só a partir do segundo a cadência é a do período.
const READ_START_DELAY: i64 = 280_000;

/// Latência de um seek. Não é proporcional à distância: o BIOS e os jogos
/// toleram folga aqui, e um valor fixo evita fingir uma precisão que não
/// temos sem modelar a mecânica do drive.
const SEEK_DELAY: i64 = 200_000;

/// Ciclos de CPU entre setores em 1x — o drive entrega 75 setores por segundo.
const CYCLES_PER_SECTOR_1X: i64 = (CPU_CLOCK_HZ / 75) as i64;

impl CdRom {
    pub fn new() -> Self {
        Self {
            index: 0,
            parameters: VecDeque::with_capacity(16),
            response: VecDeque::with_capacity(16),
            interrupt_enable: 0,
            interrupt_flags: 0,
            pending: VecDeque::new(),
            command_pending: None,
            // Sem disco: bandeja reportada como aberta, que é o que o BIOS
            // espera para cair no shell.
            status: status::SHELL_OPEN,
            mode: 0,
            disc: None,
            adpcm_output: Vec::new(),
            adpcm_rate: 37_800,
            adpcm_history: [adpcm::History::new_const(); 2],
            adpcm_busy: false,
            filter_file: 0,
            filter_channel: 0,
            seek_target: Msf::default(),
            position: Msf::default(),
            read_lba: 0,
            drive: Drive::Idle,
            next_sector_in: 0,
            delivery_retry: false,
            sector_due: false,
            sector_buffer: Vec::new(),
            sector_cursor: 0,
            data_requested: false,
            staged_sector: Vec::new(),
            sector_available: false,
            history: VecDeque::with_capacity(16),
            header_valid: false,
            sectors_delivered: 0,
            sectors_collected: 0,
            unimplemented: 0,
            last_unimplemented: 0,
        }
    }

    /// Insere uma imagem de disco no drive.
    pub fn insert_disc(&mut self, disc: Disc) {
        self.disc = Some(disc);
        self.status = status::MOTOR_ON;
    }

    /// Abre a bandeja e descarta a imagem.
    pub fn eject(&mut self) {
        self.disc = None;
        self.drive = Drive::Idle;
        self.status = status::SHELL_OPEN;
    }

    pub const fn has_disc(&self) -> bool {
        self.disc.is_some()
    }

    pub fn disc(&self) -> Option<&Disc> {
        self.disc.as_ref()
    }

    pub const fn unimplemented_commands(&self) -> u64 {
        self.unimplemented
    }

    /// Retrato do estado interno, para diagnóstico.
    ///
    /// Quando um jogo trava esperando dados, a pergunta é sempre a mesma: o
    /// drive ainda está lendo, e há resposta presa na fila sem acknowledge?
    pub fn debug_state(&self) -> String {
        format!(
            "drive={:?} setores={} recolhidos={} lba={} pendentes={} flags={:#04X} enable={:#04X} bfrd={} novo={} fifo={}/{} modo={:#04X}",
            self.drive,
            self.sectors_delivered,
            self.sectors_collected,
            self.read_lba,
            self.pending.len(),
            self.interrupt_flags,
            self.interrupt_enable,
            self.data_requested,
            self.sector_available,
            self.sector_cursor,
            self.sector_buffer.len(),
            self.mode,
        ) + &format!(
            " histórico=[{}]",
            self.history
                .iter()
                .map(|command| format!("{command:#04X}"))
                .collect::<Vec<_>>()
                .join(" ")
        )
    }

    pub const fn last_unimplemented_command(&self) -> u8 {
        self.last_unimplemented
    }

    /// Ciclos entre setores, conforme a velocidade selecionada em `Setmode`.
    const fn cycles_per_sector(&self) -> i64 {
        if self.mode & mode::DOUBLE_SPEED != 0 {
            CYCLES_PER_SECTOR_1X / 2
        } else {
            CYCLES_PER_SECTOR_1X
        }
    }

    /// Avança o controlador em `cycles` ciclos de CPU.
    pub fn step(&mut self, cycles: u32, irq: &mut IrqController) {
        // A leitura continua correndo mesmo com uma IRQ pendente; o que não
        // pode é entregar duas respostas sem o acknowledge entre elas.
        self.advance_read(cycles);

        // Só entrega a próxima resposta quando a anterior foi reconhecida.
        if self.interrupt_flags == 0 {
            if let Some(front) = self.pending.front_mut() {
                front.delay -= cycles as i64;
                if front.delay <= 0 {
                    let ready = self.pending.pop_front().expect("front existe");
                    self.response.clear();
                    self.response.extend(ready.bytes);
                    self.interrupt_flags = ready.interrupt as u8;
                }
            }
        }

        // Com a linha limpa e nada a entregar, o comando retido finalmente
        // vai ao controlador. A ordem importa: uma INT1 ou INT2 gerada neste
        // mesmo passo tem precedência e adia o comando mais uma volta.
        if self.interrupt_flags == 0 && self.pending.is_empty() {
            if let Some((command, parameters)) = self.command_pending.take() {
                self.execute(command, parameters);
            }
        }

        // A linha é publicada a cada passo, e não pulsada no instante da
        // entrega. O controlador de IRQ é quem enxerga a borda.
        //
        // A diferença aparece quando o software **habilita** uma fonte cuja
        // flag já estava acesa: com o pulso, aquela interrupção se perdia para
        // sempre, e como o controlador não entrega a resposta seguinte
        // enquanto a flag não for reconhecida, o CD-ROM parava de vez.
        irq.set_level(
            Interrupt::CdRom,
            self.interrupt_enable & self.interrupt_flags != 0,
        );
    }

    /// O que o drive faz com o setor em `lba`.
    ///
    /// PSX-SPX — "Data/ADPCM Sector Filtering/Delivery": o controlador tenta
    /// primeiro entregar ao decodificador ADPCM e, se não couber ali, ao CPU;
    /// se nenhum dos dois aceitar, o setor é descartado em silêncio.
    ///
    /// `retry` diz se esta é a **segunda** tentativa de entrega, depois de a
    /// primeira ter esbarrado numa interrupção pendente. A diferença importa:
    /// só a segunda confere arquivo e canal. O hardware é assim, e é o que
    /// permite a um jogo isolar um canal de um arquivo STR multiplexado.
    fn classify(&self, lba: u32, retry: bool) -> Delivery {
        let Some([file, channel, flags, _]) =
            self.disc.as_ref().and_then(|disc| disc.subheader(lba))
        else {
            // Sem subheader não é Mode 2: só pode ser dado.
            return Delivery::Data;
        };

        const AUDIO_SECTOR: u8 = submode::AUDIO | submode::REAL_TIME;
        let is_audio = flags & AUDIO_SECTOR == AUDIO_SECTOR;
        let filtering = self.mode & mode::XA_FILTER != 0;
        let matches_filter = file == self.filter_file && channel == self.filter_channel;

        if self.mode & mode::XA_ADPCM != 0 && is_audio && (!filtering || matches_filter) {
            return Delivery::Adpcm;
        }
        // Com o filtro ligado, áudio em tempo real nunca vira dado — nem
        // quando o ADPCM está desligado ou o canal não casa.
        if filtering && is_audio {
            return Delivery::Discard;
        }
        if retry && filtering && !matches_filter {
            return Delivery::Discard;
        }
        Delivery::Data
    }

    /// Decodifica um setor de áudio XA e o põe na fila para a SPU.
    ///
    /// O drive faz isso sozinho: o som sai pelo mixer sem passar pela CPU e
    /// sem ocupar uma voz. Um jogo que sincroniza vídeo pelo áudio depende
    /// disso para avançar.
    fn decode_adpcm(&mut self, lba: u32) {
        let Some(disc) = self.disc.as_ref() else {
            return;
        };
        let Some([_, _, _, coding]) = disc.subheader(lba) else {
            return;
        };
        let Some(sector) = disc.read_whole_sector(lba) else {
            return;
        };
        // Em "setor inteiro" os bytes começam no header: 4 de header e 8 de
        // subheader antes da carga.
        let payload = &sector[12..];
        let coding = XaCoding::from_byte(coding);
        self.adpcm_rate = coding.sample_rate;
        adpcm::decode_xa_sector(
            payload,
            coding,
            &mut self.adpcm_history,
            &mut self.adpcm_output,
        );
    }

    /// Informa se o decodificador ainda tem áudio para tocar.
    ///
    /// Vira o bit `ADPBUSY` de `0x1F80_1800`, que é como o software pergunta
    /// se o XA ainda está rodando.
    pub fn set_adpcm_busy(&mut self, busy: bool) {
        self.adpcm_busy = busy;
    }

    /// Retira o áudio de CD decodificado desde a última chamada.
    ///
    /// O barramento o entrega à SPU; o CD-ROM não a conhece.
    pub fn take_audio(&mut self) -> Option<(Vec<(i16, i16)>, u32)> {
        if self.adpcm_output.is_empty() {
            return None;
        }
        Some((std::mem::take(&mut self.adpcm_output), self.adpcm_rate))
    }

    /// Produz o próximo setor quando o drive está lendo.
    fn advance_read(&mut self, cycles: u32) {
        if self.drive != Drive::Reading {
            return;
        }

        // A cadência é do motor, não do software.
        //
        // O prazo do próximo setor é reagendado assim que vence, aconteça o
        // que acontecer com a entrega. Zerar o contador enquanto a INT1
        // anterior não fosse reconhecida — como fazíamos — fazia o setor
        // seguinte disparar no mesmo instante do acknowledge, fechando a
        // janela que o PSX-SPX descreve em "CDROM Incoming Data": *"there
        // seems to be a small delay between the acknowledge and the next
        // interrupt, and Data Requests during that period are still treated to
        // belong to the old interrupt"*. Sem essa janela o jogo nunca alcança
        // o próprio pedido de dados, e o fluxo trava com o drive girando.
        self.next_sector_in -= cycles as i64;
        if self.next_sector_in <= 0 {
            self.next_sector_in += self.cycles_per_sector();
            self.sector_due = true;
        }
        if !self.sector_due {
            return;
        }

        // Com a INT1 anterior ainda acesa o setor espera no buffer, e a
        // tentativa gasta conta para a filtragem por arquivo e canal.
        if self.interrupt_flags != 0 {
            self.delivery_retry = true;
            return;
        }
        self.sector_due = false;
        let retry = std::mem::replace(&mut self.delivery_retry, false);

        let whole = self.mode & mode::WHOLE_SECTOR != 0;
        let lba = self.read_lba;

        // Um setor de áudio XA não chega ao CPU: o drive o encaminha ao
        // decodificador ADPCM, que toca o som sem passar pela CPU. Entregá-lo
        // como se fosse dado enche o jogo de INT1 que ele não pediu — o
        // Grandstream Saga recebe mais setores de áudio do que de dados.
        match self.classify(lba, retry) {
            Delivery::Adpcm => {
                self.decode_adpcm(lba);
                self.position = Msf::from_lba(lba);
                self.read_lba = lba.wrapping_add(1);
                return;
            }
            Delivery::Discard => {
                self.position = Msf::from_lba(lba);
                self.read_lba = lba.wrapping_add(1);
                return;
            }
            Delivery::Data => {}
        }
        let sector = self.disc.as_ref().and_then(|disc| {
            if whole {
                disc.read_whole_sector(lba)
            } else {
                disc.read_user_data(lba)
            }
        });

        match sector {
            Some(bytes) => {
                self.staged_sector.clear();
                self.staged_sector.extend_from_slice(bytes);
                self.sector_available = true;
                self.sectors_delivered += 1;
                self.position = Msf::from_lba(lba);
                self.header_valid = true;
                // Chegou o primeiro setor: o posicionamento acabou.
                self.status &= !status::SEEKING;
                self.status |= status::READING;
                self.read_lba = lba.wrapping_add(1);
                let status = self.status;
                self.schedule(CdInterrupt::DataReady, vec![status], 0);
            }
            None => {
                // Passou do fim do disco (ou de uma faixa de áudio, que ainda
                // não sabemos entregar): o hardware responde INT4 e para.
                self.drive = Drive::Idle;
                self.status &= !status::READING;
                let status = self.status;
                self.schedule(CdInterrupt::DataEnd, vec![status], 0);
            }
        }
    }

    /// Leitura de `0x1F80_1800..0x1F80_1803`.
    pub fn read(&mut self, offset: u32) -> u8 {
        match offset & 3 {
            0 => self.status_register(),
            1 => self.response.pop_front().unwrap_or(0),
            2 => self.read_data_byte(),
            _ => match self.index {
                0 | 2 => self.interrupt_enable | 0xE0,
                _ => self.interrupt_flags | 0xE0,
            },
        }
    }

    /// Um byte da FIFO de dados do setor.
    fn read_data_byte(&mut self) -> u8 {
        let byte = self
            .sector_buffer
            .get(self.sector_cursor)
            .copied()
            .unwrap_or(0);
        if self.sector_cursor < self.sector_buffer.len() {
            self.sector_cursor += 1;
        }
        byte
    }

    /// Uma palavra da FIFO de dados, para o canal 3 do DMA.
    pub fn dma_read(&mut self) -> u32 {
        let mut word = 0u32;
        for shift in [0, 8, 16, 24] {
            word |= (self.read_data_byte() as u32) << shift;
        }
        word
    }

    /// `true` enquanto houver bytes de setor para entregar.
    pub fn data_available(&self) -> bool {
        self.data_requested && self.sector_cursor < self.sector_buffer.len()
    }

    /// `0x1F80_1800` — índice e bits de prontidão.
    fn status_register(&self) -> u8 {
        let mut value = self.index;
        // Bit 7: o controlador ainda não aceitou o comando escrito.
        if self.command_pending.is_some() {
            value |= 1 << 7;
        }
        // Bit 2: o decodificador de áudio XA está tocando.
        if self.adpcm_busy {
            value |= 1 << 2;
        }
        // Bit 3: FIFO de parâmetros vazia (ativo em 1).
        if self.parameters.is_empty() {
            value |= 1 << 3;
        }
        // Bit 4: FIFO de parâmetros não está cheia.
        if self.parameters.len() < 16 {
            value |= 1 << 4;
        }
        // Bit 5: FIFO de resposta tem dados.
        if !self.response.is_empty() {
            value |= 1 << 5;
        }
        // Bit 6: FIFO de dados tem dados.
        if self.data_available() {
            value |= 1 << 6;
        }
        value
    }

    /// Escrita em `0x1F80_1800..0x1F80_1803`.
    pub fn write(&mut self, offset: u32, value: u8) {
        match offset & 3 {
            0 => self.index = value & 3,
            // Índices 1..3 escrevem os volumes do mixer de CD-DA, que ainda
            // não misturamos. Ignorar é o comportamento certo: são
            // registradores de som, não de controle.
            1 => {
                if self.index == 0 {
                    let parameters = self.parameters.drain(..).collect();
                    self.command_pending = Some((value, parameters));
                }
            }
            2 => match self.index {
                0 => {
                    if self.parameters.len() < 16 {
                        self.parameters.push_back(value);
                    }
                }
                1 => self.interrupt_enable = value & 0x1F,
                _ => {}
            },
            _ => match self.index {
                0 => self.set_data_request(value),
                1 => {
                    // Acknowledge: escrever 1 nos bits limpa as flags.
                    self.interrupt_flags &= !(value & 0x1F);
                    if value & 0x1F != 0 {
                        // PSX-SPX, HCLRCTL: *"After acknowledge, the result
                        // FIFO is drained"*. Sem isso, os bytes que o software
                        // não leu de uma resposta longa ficam para trás e
                        // mantêm o `RSLRRDY` do status aceso para sempre — o
                        // jogo passa a ver "ainda há resposta" a cada volta do
                        // laço e nunca chega a pedir os dados do setor.
                        self.response.clear();
                    }
                    if value & 0x40 != 0 {
                        self.parameters.clear();
                    }
                }
                _ => {}
            },
        }
    }

    /// `0x1F80_1803.Index0` — o bit 7 (BFRD) move o setor para a FIFO.
    fn set_data_request(&mut self, value: u8) {
        if value & 0x80 != 0 {
            // Cada INT1 disponibiliza **um** setor; o pedido move esse setor
            // para a FIFO e rebobina o cursor.
            //
            // Condicionar a recarga a ter drenado a FIFO seria errado: em modo
            // "setor inteiro" o CPU lê o header, o subheader e os 2048 bytes
            // de dados e para, deixando os 280 bytes de ECC para trás. Com a
            // recarga presa nesse resto, o setor seguinte nunca chegaria e o
            // jogo esperaria para sempre por dados que já estavam prontos.
            if self.sector_available {
                std::mem::swap(&mut self.sector_buffer, &mut self.staged_sector);
                self.sector_cursor = 0;
                self.sector_available = false;
                self.sectors_collected += 1;
            }
            self.data_requested = true;
        } else {
            self.data_requested = false;
            self.sector_buffer.clear();
            self.sector_cursor = 0;
        }
    }

    fn schedule(&mut self, interrupt: CdInterrupt, bytes: Vec<u8>, delay: i64) {
        self.pending.push_back(PendingResponse {
            delay,
            interrupt,
            bytes,
        });
    }

    /// Agenda o acknowledge padrão: INT3 com o status corrente.
    fn acknowledge(&mut self) {
        let status = self.status;
        self.schedule(CdInterrupt::Acknowledge, vec![status], ACKNOWLEDGE_DELAY);
    }

    /// Agenda INT5 com o código de erro.
    fn error(&mut self, code: u8) {
        let status = self.status | status::ERROR;
        self.schedule(CdInterrupt::Error, vec![status, code], ACKNOWLEDGE_DELAY);
    }

    /// Executa um comando escrito em `0x1F80_1801.Index0`.
    fn execute(&mut self, command: u8, parameters: Vec<u8>) {
        if self.history.len() == 16 {
            self.history.pop_front();
        }
        self.history.push_back(command);

        match command {
            // GetStat
            0x01 => self.acknowledge(),

            // Setloc — alvo do próximo seek, em BCD.
            0x02 => {
                if let [minute, second, frame, ..] = parameters[..] {
                    self.seek_target = Msf::from_bcd(minute, second, frame);
                    self.acknowledge();
                } else {
                    self.error(0x20);
                }
            }

            // ReadN / ReadS — leitura contínua a partir do alvo.
            0x06 | 0x1B => {
                if self.disc.is_none() {
                    self.error(0x80);
                    return;
                }
                self.read_lba = self.seek_target.to_lba();
                self.drive = Drive::Reading;
                // O drive primeiro posiciona e só então lê: até o primeiro
                // setor chegar, o status reporta `seeking`, não `reading`.
                self.status |= status::SEEKING;
                self.status &= !status::READING;
                self.next_sector_in = self.cycles_per_sector() + READ_START_DELAY;
                self.acknowledge();
            }

            // MotorOn — liga o motor. Responde duas vezes.
            0x07 => {
                self.status |= status::MOTOR_ON;
                self.acknowledge();
                let status = self.status;
                self.schedule(CdInterrupt::Complete, vec![status], ACKNOWLEDGE_DELAY * 5);
            }

            // Stop — para o motor.
            0x08 => {
                self.drive = Drive::Idle;
                self.status &= !(status::READING | status::SEEKING | status::MOTOR_ON);
                self.acknowledge();
                let status = self.status;
                self.schedule(CdInterrupt::Complete, vec![status], ACKNOWLEDGE_DELAY * 5);
            }

            // Pause — para a leitura, mantendo a posição.
            0x09 => {
                self.drive = Drive::Idle;
                self.status &= !status::READING;
                let status = self.status;
                self.schedule(CdInterrupt::Acknowledge, vec![status], PAUSE_ACK_DELAY);
                self.schedule(CdInterrupt::Complete, vec![status], PAUSE_COMPLETE_DELAY);
            }

            // Init — reset do controlador.
            0x0A => {
                self.mode = 0;
                self.header_valid = false;
                self.drive = Drive::Idle;
                self.status = if self.has_disc() {
                    status::MOTOR_ON
                } else {
                    status::SHELL_OPEN
                };
                let status = self.status;
                self.schedule(CdInterrupt::Acknowledge, vec![status], INIT_ACK_DELAY);
                self.schedule(CdInterrupt::Complete, vec![status], INIT_COMPLETE_DELAY);
            }

            // Mute / Demute — só afetam o mixer de CD-DA.
            0x0B | 0x0C => self.acknowledge(),

            // Setmode
            0x0E => {
                if let Some(&value) = parameters.first() {
                    self.mode = value;
                    self.acknowledge();
                } else {
                    self.error(0x20);
                }
            }

            // Setfilter — escolhe o arquivo e o canal XA a aceitar.
            0x0D => {
                if let [file, channel, ..] = parameters[..] {
                    self.filter_file = file;
                    self.filter_channel = channel;
                }
                self.acknowledge();
            }

            // Getparam — devolve modo e filtro correntes.
            0x0F => {
                let status = self.status;
                let mode = self.mode;
                let bytes = vec![status, mode, 0x00, 0x00, 0x00];
                self.schedule(CdInterrupt::Acknowledge, bytes, ACKNOWLEDGE_DELAY);
            }

            // SetSession — só a sessão 1 existe num disco de jogo.
            0x12 => {
                self.acknowledge();
                let status = self.status;
                self.schedule(CdInterrupt::Complete, vec![status], SEEK_DELAY);
            }

            // GetlocL — posição e header do último setor lido.
            0x10 => {
                if !self.header_valid {
                    self.error(0x80);
                    return;
                }
                let [minute, second, frame] = self.position.to_bcd();
                // Os três últimos bytes são o subheader; sem XA eles são zero.
                let bytes = vec![minute, second, frame, 0x02, 0x00, 0x00, 0x00, 0x00];
                self.schedule(CdInterrupt::Acknowledge, bytes, ACKNOWLEDGE_DELAY);
            }

            // GetlocP — posição em coordenadas de faixa.
            0x11 => {
                let [minute, second, frame] = self.position.to_bcd();
                // track, index, MSF relativo à faixa, MSF absoluto. Com uma
                // faixa de dados só, o relativo é igual ao absoluto.
                let bytes = vec![0x01, 0x01, minute, second, frame, minute, second, frame];
                self.schedule(CdInterrupt::Acknowledge, bytes, ACKNOWLEDGE_DELAY);
            }

            // GetTN — primeira e última faixa.
            0x13 => match self.disc.as_ref() {
                Some(disc) => {
                    let (first, last) = disc.track_range();
                    let status = self.status;
                    let bytes = vec![status, to_bcd(first), to_bcd(last)];
                    self.schedule(CdInterrupt::Acknowledge, bytes, ACKNOWLEDGE_DELAY);
                }
                None => self.error(0x80),
            },

            // GetTD — início de uma faixa.
            0x14 => {
                let track = parameters.first().copied().unwrap_or(0);
                let start = self
                    .disc
                    .as_ref()
                    .and_then(|disc| disc.track_start(from_bcd(track)));
                match start {
                    Some(msf) => {
                        let [minute, second, _] = msf.to_bcd();
                        let status = self.status;
                        let bytes = vec![status, minute, second];
                        self.schedule(CdInterrupt::Acknowledge, bytes, ACKNOWLEDGE_DELAY);
                    }
                    None => self.error(0x10),
                }
            }

            // SeekL / SeekP — posiciona a cabeça no alvo do Setloc.
            0x15 | 0x16 => {
                let Some(disc) = self.disc.as_ref() else {
                    self.error(0x80);
                    return;
                };
                // Posicionar além do fim do disco é erro de seek: a cabeça não
                // tem onde pousar, e o hardware responde INT5 em vez de fingir
                // que chegou.
                if self.seek_target.to_lba() >= disc.total_sectors() {
                    self.header_valid = false;
                    self.status |= status::SEEK_ERROR;
                    self.error(0x04);
                    return;
                }
                self.status &= !status::SEEK_ERROR;
                // `SeekL` (0x15) posiciona **em modo de dados** e lê o header do
                // setor de destino, então `GetlocL` passa a responder. `SeekP`
                // (0x16) é seek de áudio: posiciona sem ler header.
                self.header_valid = command == 0x15;
                self.position = self.seek_target;
                self.read_lba = self.seek_target.to_lba();
                self.acknowledge();
                let status = self.status;
                self.schedule(CdInterrupt::Complete, vec![status], SEEK_DELAY);
            }

            // Test
            0x19 => match parameters.first() {
                // Versão do controlador (data de build do firmware).
                Some(0x20) => self.schedule(
                    CdInterrupt::Acknowledge,
                    vec![0x94, 0x09, 0x19, 0xC0],
                    ACKNOWLEDGE_DELAY,
                ),
                _ => self.acknowledge(),
            },

            // GetID — identifica o disco e a região.
            0x1A => {
                self.acknowledge();
                match self.disc.as_ref() {
                    Some(disc) => {
                        let region = disc.region().id_bytes();
                        let bytes = vec![
                            0x02, 0x00, 0x20, 0x00, region[0], region[1], region[2], region[3],
                        ];
                        self.schedule(CdInterrupt::Complete, bytes, ACKNOWLEDGE_DELAY * 3);
                    }
                    None => {
                        // INT5 com "no disc".
                        self.schedule(
                            CdInterrupt::Error,
                            vec![0x08, 0x40, 0, 0, 0, 0, 0, 0],
                            ACKNOWLEDGE_DELAY * 3,
                        );
                    }
                }
            }

            // ReadTOC — relê a tabela de conteúdo, que já temos em memória.
            0x1E => {
                self.acknowledge();
                let status = self.status;
                self.schedule(CdInterrupt::Complete, vec![status], ACKNOWLEDGE_DELAY * 20);
            }

            _ => {
                self.unimplemented += 1;
                self.last_unimplemented = command;
                // Responder com erro é melhor que travar: o BIOS trata INT5.
                self.error(0x40);
            }
        }
    }
}

const fn to_bcd(value: u8) -> u8 {
    ((value / 10) << 4) | (value % 10)
}

const fn from_bcd(value: u8) -> u8 {
    (value >> 4) * 10 + (value & 0x0F)
}

impl Default for CdRom {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drive() -> (CdRom, IrqController) {
        let mut irq = IrqController::new();
        irq.write_mask(0x07FF);
        (CdRom::new(), irq)
    }

    /// Habilita todas as IRQs e devolve o drive pronto para receber comandos.
    fn armed() -> (CdRom, IrqController) {
        let (mut cdrom, irq) = drive();
        cdrom.write(0, 1);
        cdrom.write(2, 0x1F);
        cdrom.write(0, 0);
        (cdrom, irq)
    }

    /// Envia um comando com parâmetros.
    fn command(cdrom: &mut CdRom, command: u8, parameters: &[u8]) {
        cdrom.write(0, 0);
        for &parameter in parameters {
            cdrom.write(2, parameter);
        }
        cdrom.write(1, command);
    }

    /// Espera a próxima IRQ e devolve o seu código.
    fn wait_irq(cdrom: &mut CdRom, irq: &mut IrqController) -> u8 {
        for _ in 0..400 {
            cdrom.step(20_000, irq);
            if cdrom.interrupt_flags != 0 {
                return cdrom.interrupt_flags;
            }
        }
        0
    }

    /// Reconhece a IRQ corrente, liberando a próxima resposta.
    fn ack(cdrom: &mut CdRom) {
        cdrom.write(0, 1);
        cdrom.write(3, 0x07);
        cdrom.write(0, 0);
    }

    /// Um disco de teste com `sectors` setores, cada um preenchido com o seu
    /// próprio número de LBA.
    fn test_disc(sectors: usize) -> Disc {
        let mut image = vec![0u8; SECTOR_USER * sectors];
        for lba in 0..sectors {
            image[lba * SECTOR_USER] = lba as u8;
        }
        Disc::from_image(image).expect("imagem de teste")
    }

    #[test]
    fn index_selects_the_meaning_of_the_other_ports() {
        let (mut cdrom, _) = drive();
        cdrom.write(0, 1);
        assert_eq!(cdrom.read(0) & 3, 1);
        cdrom.write(0, 3);
        assert_eq!(cdrom.read(0) & 3, 3);
        // Só os dois bits baixos são gravados.
        cdrom.write(0, 0xFF);
        assert_eq!(cdrom.read(0) & 3, 3);
    }

    #[test]
    fn getstat_answers_after_the_command_delay() {
        let (mut cdrom, mut irq) = armed();
        cdrom.write(1, 0x01); // GetStat

        // Antes do delay, nada aconteceu.
        cdrom.step(1000, &mut irq);
        assert_eq!(irq.stat() & (1 << Interrupt::CdRom as u16), 0);

        cdrom.step(ACKNOWLEDGE_DELAY as u32, &mut irq);
        assert_ne!(irq.stat() & (1 << Interrupt::CdRom as u16), 0);

        cdrom.write(0, 1);
        assert_eq!(cdrom.read(3) & 0x1F, CdInterrupt::Acknowledge as u8);
        assert_eq!(cdrom.read(1), status::SHELL_OPEN);
    }

    #[test]
    fn acknowledge_clears_the_flag_and_releases_the_next_response() {
        let (mut cdrom, mut irq) = armed();
        cdrom.write(1, 0x0A); // Init: duas respostas

        assert_eq!(
            wait_irq(&mut cdrom, &mut irq),
            CdInterrupt::Acknowledge as u8
        );

        // Sem acknowledge, a segunda resposta não chega.
        for _ in 0..200 {
            cdrom.step(20_000, &mut irq);
        }
        cdrom.write(0, 1);
        assert_eq!(cdrom.read(3) & 0x1F, CdInterrupt::Acknowledge as u8);

        cdrom.write(3, 0x07);
        assert_eq!(cdrom.read(3) & 0x1F, 0);
        cdrom.write(0, 0);

        assert_eq!(wait_irq(&mut cdrom, &mut irq), CdInterrupt::Complete as u8);
    }

    #[test]
    fn getid_without_disc_reports_an_error() {
        let (mut cdrom, mut irq) = armed();
        cdrom.write(1, 0x1A); // GetID

        assert_eq!(
            wait_irq(&mut cdrom, &mut irq),
            CdInterrupt::Acknowledge as u8
        );
        ack(&mut cdrom);

        assert_eq!(wait_irq(&mut cdrom, &mut irq), CdInterrupt::Error as u8);
        assert_eq!(cdrom.read(1), 0x08, "primeiro byte indica 'no disc'");
    }

    #[test]
    fn getid_with_disc_reports_the_region_of_the_image() {
        let (mut cdrom, mut irq) = armed();
        let mut image = vec![0u8; SECTOR_USER * 20];
        let text = b"Licensed by Sony Computer Entertainment Europe";
        image[SECTOR_USER * 4..SECTOR_USER * 4 + text.len()].copy_from_slice(text);
        cdrom.insert_disc(Disc::from_image(image).unwrap());

        cdrom.write(1, 0x1A);
        assert_eq!(
            wait_irq(&mut cdrom, &mut irq),
            CdInterrupt::Acknowledge as u8
        );
        ack(&mut cdrom);
        assert_eq!(wait_irq(&mut cdrom, &mut irq), CdInterrupt::Complete as u8);

        let response: Vec<u8> = (0..8).map(|_| cdrom.read(1)).collect();
        assert_eq!(&response[4..8], b"SCEE");
    }

    #[test]
    fn parameter_fifo_reports_empty_and_not_full() {
        let (mut cdrom, _) = drive();
        assert_ne!(cdrom.read(0) & (1 << 3), 0, "FIFO vazia");
        assert_ne!(cdrom.read(0) & (1 << 4), 0, "FIFO não cheia");

        cdrom.write(2, 0x20);
        assert_eq!(cdrom.read(0) & (1 << 3), 0, "FIFO deixou de estar vazia");
    }

    #[test]
    fn test_command_returns_the_firmware_version() {
        let (mut cdrom, mut irq) = armed();
        command(&mut cdrom, 0x19, &[0x20]); // Test, subfunção 0x20

        assert_eq!(
            wait_irq(&mut cdrom, &mut irq),
            CdInterrupt::Acknowledge as u8
        );
        assert_eq!(cdrom.read(1), 0x94);
        assert_eq!(cdrom.read(1), 0x09);
    }

    #[test]
    fn unknown_command_is_counted_and_answered_with_an_error() {
        let (mut cdrom, mut irq) = armed();
        command(&mut cdrom, 0x7E, &[]);

        // O comando fica retido até o controlador aceitá-lo, então o contador
        // só sobe depois do primeiro passo.
        assert_eq!(wait_irq(&mut cdrom, &mut irq), CdInterrupt::Error as u8);
        assert_eq!(cdrom.unimplemented_commands(), 1);
        assert_eq!(cdrom.last_unimplemented_command(), 0x7E);
    }

    #[test]
    fn a_command_waits_for_the_pending_interrupt_to_be_acknowledged() {
        let (mut cdrom, mut irq) = armed();
        command(&mut cdrom, 0x01, &[]); // GetStat
        assert_eq!(
            wait_irq(&mut cdrom, &mut irq),
            CdInterrupt::Acknowledge as u8
        );

        // Com a interrupção ainda acesa, o comando seguinte não é executado.
        command(&mut cdrom, 0x19, &[0x20]);
        for _ in 0..8 {
            cdrom.step(ACKNOWLEDGE_DELAY as u32, &mut irq);
        }
        assert_eq!(
            cdrom.read(1),
            cdrom.status,
            "a resposta ainda é a do GetStat"
        );

        ack(&mut cdrom);
        assert_eq!(
            wait_irq(&mut cdrom, &mut irq),
            CdInterrupt::Acknowledge as u8
        );
        assert_eq!(cdrom.read(1), 0x94, "agora sim o Test respondeu");
    }

    #[test]
    fn inserting_a_disc_closes_the_shell_and_spins_the_motor() {
        let (mut cdrom, _) = drive();
        assert_ne!(cdrom.status & status::SHELL_OPEN, 0);

        cdrom.insert_disc(test_disc(4));
        assert_eq!(cdrom.status & status::SHELL_OPEN, 0);
        assert_ne!(cdrom.status & status::MOTOR_ON, 0);
        assert!(cdrom.has_disc());

        cdrom.eject();
        assert_ne!(cdrom.status & status::SHELL_OPEN, 0);
        assert!(!cdrom.has_disc());
    }

    #[test]
    fn setloc_then_read_delivers_the_addressed_sector() {
        let (mut cdrom, mut irq) = armed();
        cdrom.insert_disc(test_disc(10));

        // LBA 3 = MSF 00:02:03 em BCD.
        command(&mut cdrom, 0x02, &[0x00, 0x02, 0x03]);
        assert_eq!(
            wait_irq(&mut cdrom, &mut irq),
            CdInterrupt::Acknowledge as u8
        );
        ack(&mut cdrom);

        command(&mut cdrom, 0x06, &[]); // ReadN
        assert_eq!(
            wait_irq(&mut cdrom, &mut irq),
            CdInterrupt::Acknowledge as u8
        );
        ack(&mut cdrom);

        assert_eq!(
            wait_irq(&mut cdrom, &mut irq),
            CdInterrupt::DataReady as u8,
            "o setor tem que chegar como INT1"
        );

        // BFRD move o setor para a FIFO.
        cdrom.write(0, 0);
        cdrom.write(3, 0x80);
        assert!(cdrom.data_available());
        assert_ne!(cdrom.read(0) & (1 << 6), 0, "bit de dados prontos");
        assert_eq!(cdrom.read(2), 3, "o primeiro byte identifica o LBA 3");
    }

    #[test]
    fn reading_continues_sector_after_sector() {
        let (mut cdrom, mut irq) = armed();
        cdrom.insert_disc(test_disc(10));

        command(&mut cdrom, 0x02, &[0x00, 0x02, 0x00]); // LBA 0
        wait_irq(&mut cdrom, &mut irq);
        ack(&mut cdrom);
        command(&mut cdrom, 0x06, &[]);
        wait_irq(&mut cdrom, &mut irq);
        ack(&mut cdrom);

        for expected in 0..4u8 {
            assert_eq!(
                wait_irq(&mut cdrom, &mut irq),
                CdInterrupt::DataReady as u8,
                "setor {expected}"
            );
            cdrom.write(0, 0);
            cdrom.write(3, 0x80);
            assert_eq!(cdrom.read(2), expected, "conteúdo do setor {expected}");
            // Drena o resto antes do próximo, como o CPU faria.
            cdrom.write(3, 0x00);
            ack(&mut cdrom);
        }
    }

    #[test]
    fn reading_past_the_end_of_the_disc_reports_data_end() {
        let (mut cdrom, mut irq) = armed();
        cdrom.insert_disc(test_disc(2));

        command(&mut cdrom, 0x02, &[0x00, 0x02, 0x00]);
        wait_irq(&mut cdrom, &mut irq);
        ack(&mut cdrom);
        command(&mut cdrom, 0x06, &[]);
        wait_irq(&mut cdrom, &mut irq);
        ack(&mut cdrom);

        // Dois setores válidos...
        for _ in 0..2 {
            assert_eq!(wait_irq(&mut cdrom, &mut irq), CdInterrupt::DataReady as u8);
            ack(&mut cdrom);
        }
        // ...e o fim do disco.
        assert_eq!(wait_irq(&mut cdrom, &mut irq), CdInterrupt::DataEnd as u8);
        assert_eq!(cdrom.status & status::READING, 0, "a leitura parou");
    }

    #[test]
    fn read_without_disc_answers_an_error() {
        let (mut cdrom, mut irq) = armed();
        command(&mut cdrom, 0x06, &[]);
        assert_eq!(wait_irq(&mut cdrom, &mut irq), CdInterrupt::Error as u8);
    }

    #[test]
    fn setmode_selects_double_speed() {
        let (mut cdrom, mut irq) = armed();
        assert_eq!(cdrom.cycles_per_sector(), CYCLES_PER_SECTOR_1X);

        command(&mut cdrom, 0x0E, &[mode::DOUBLE_SPEED]);
        wait_irq(&mut cdrom, &mut irq);

        assert_eq!(cdrom.cycles_per_sector(), CYCLES_PER_SECTOR_1X / 2);
    }

    #[test]
    fn whole_sector_mode_delivers_the_header_too() {
        let (mut cdrom, mut irq) = armed();
        // Uma imagem crua, que é a única que tem header para entregar.
        let mut image = vec![0u8; SECTOR_RAW * 2];
        image[..12].copy_from_slice(&[
            0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00,
        ]);
        image[15] = 1; // Mode 1
        cdrom.insert_disc(Disc::from_image(image).unwrap());

        command(&mut cdrom, 0x0E, &[mode::WHOLE_SECTOR]);
        wait_irq(&mut cdrom, &mut irq);
        ack(&mut cdrom);
        command(&mut cdrom, 0x02, &[0x00, 0x02, 0x00]);
        wait_irq(&mut cdrom, &mut irq);
        ack(&mut cdrom);
        command(&mut cdrom, 0x06, &[]);
        wait_irq(&mut cdrom, &mut irq);
        ack(&mut cdrom);

        assert_eq!(wait_irq(&mut cdrom, &mut irq), CdInterrupt::DataReady as u8);
        cdrom.write(0, 0);
        cdrom.write(3, 0x80);
        // 2340 bytes: o setor inteiro menos o sync de 12.
        assert_eq!(cdrom.sector_buffer.len(), SECTOR_RAW - 12);
    }

    #[test]
    fn gettn_reports_the_track_range() {
        let (mut cdrom, mut irq) = armed();
        cdrom.insert_disc(test_disc(4));

        command(&mut cdrom, 0x13, &[]);
        assert_eq!(
            wait_irq(&mut cdrom, &mut irq),
            CdInterrupt::Acknowledge as u8
        );

        let _status = cdrom.read(1);
        assert_eq!(cdrom.read(1), 0x01, "primeira faixa");
        assert_eq!(cdrom.read(1), 0x01, "última faixa");
    }

    #[test]
    fn dma_reads_four_bytes_per_word() {
        let (mut cdrom, mut irq) = armed();
        let mut image = vec![0u8; SECTOR_USER * 2];
        image[..4].copy_from_slice(&[0x11, 0x22, 0x33, 0x44]);
        cdrom.insert_disc(Disc::from_image(image).unwrap());

        command(&mut cdrom, 0x02, &[0x00, 0x02, 0x00]);
        wait_irq(&mut cdrom, &mut irq);
        ack(&mut cdrom);
        command(&mut cdrom, 0x06, &[]);
        wait_irq(&mut cdrom, &mut irq);
        ack(&mut cdrom);
        wait_irq(&mut cdrom, &mut irq);

        cdrom.write(0, 0);
        cdrom.write(3, 0x80);
        assert_eq!(cdrom.dma_read(), 0x4433_2211, "little-endian");
    }

    #[test]
    fn clearing_the_request_bit_empties_the_fifo() {
        let (mut cdrom, mut irq) = armed();
        cdrom.insert_disc(test_disc(4));

        command(&mut cdrom, 0x02, &[0x00, 0x02, 0x00]);
        wait_irq(&mut cdrom, &mut irq);
        ack(&mut cdrom);
        command(&mut cdrom, 0x06, &[]);
        wait_irq(&mut cdrom, &mut irq);
        ack(&mut cdrom);
        wait_irq(&mut cdrom, &mut irq);

        cdrom.write(0, 0);
        cdrom.write(3, 0x80);
        assert!(cdrom.data_available());

        cdrom.write(3, 0x00);
        assert!(!cdrom.data_available());
        assert_eq!(cdrom.read(0) & (1 << 6), 0);
    }

    #[test]
    fn motor_on_spins_the_drive_and_answers_twice() {
        let (mut cdrom, mut irq) = armed();
        cdrom.insert_disc(test_disc(4));
        command(&mut cdrom, 0x08, &[]); // Stop
        wait_irq(&mut cdrom, &mut irq);
        ack(&mut cdrom);
        wait_irq(&mut cdrom, &mut irq);
        ack(&mut cdrom);
        assert_eq!(cdrom.status & status::MOTOR_ON, 0);

        command(&mut cdrom, 0x07, &[]); // MotorOn
        assert_eq!(
            wait_irq(&mut cdrom, &mut irq),
            CdInterrupt::Acknowledge as u8
        );
        ack(&mut cdrom);
        assert_eq!(wait_irq(&mut cdrom, &mut irq), CdInterrupt::Complete as u8);
        assert_ne!(cdrom.status & status::MOTOR_ON, 0);
        assert_eq!(cdrom.unimplemented_commands(), 0, "MotorOn é implementado");
    }

    #[test]
    fn getparam_reports_the_mode_that_setmode_wrote() {
        let (mut cdrom, mut irq) = armed();
        command(&mut cdrom, 0x0E, &[mode::DOUBLE_SPEED | mode::WHOLE_SECTOR]);
        wait_irq(&mut cdrom, &mut irq);
        ack(&mut cdrom);

        command(&mut cdrom, 0x0F, &[]);
        assert_eq!(
            wait_irq(&mut cdrom, &mut irq),
            CdInterrupt::Acknowledge as u8
        );
        let _status = cdrom.read(1);
        assert_eq!(cdrom.read(1), mode::DOUBLE_SPEED | mode::WHOLE_SECTOR);
    }

    #[test]
    fn pause_stops_the_reading_flag() {
        let (mut cdrom, mut irq) = armed();
        cdrom.insert_disc(test_disc(10));

        command(&mut cdrom, 0x02, &[0x00, 0x02, 0x00]);
        wait_irq(&mut cdrom, &mut irq);
        ack(&mut cdrom);
        command(&mut cdrom, 0x06, &[]);
        wait_irq(&mut cdrom, &mut irq);
        ack(&mut cdrom);
        // Logo após o ReadN o drive ainda está posicionando.
        assert_ne!(cdrom.status & status::SEEKING, 0);
        assert_eq!(cdrom.status & status::READING, 0);

        // O primeiro setor troca posicionamento por leitura.
        assert_eq!(wait_irq(&mut cdrom, &mut irq), CdInterrupt::DataReady as u8);
        assert_ne!(cdrom.status & status::READING, 0);
        ack(&mut cdrom);

        command(&mut cdrom, 0x09, &[]); // Pause
        wait_irq(&mut cdrom, &mut irq);
        assert_eq!(cdrom.status & status::READING, 0);
    }
}
