//! Controlador de CD-ROM.
//!
//! Referência: PSX-SPX — "CDROM Drive", "CDROM Controller I/O Ports".
//!
//! **Escopo atual:** o *plumbing* de registradores está implementado — o
//! comportamento de índice das quatro portas, as FIFOs de parâmetro e
//! resposta, e as flags de IRQ com o acknowledge em duas etapas. O conjunto de
//! comandos cobre apenas o mínimo para o console reportar "sem disco" sem
//! travar o boot.
//!
//! A emulação completa (seek, leitura de setores, XA-ADPCM, CD-DA, parsing de
//! CUE/BIN) é entrega do agente `@cdrom`.

use std::collections::VecDeque;

use crate::irq::{Interrupt, IrqController};

/// Códigos de interrupção que o controlador entrega em `0x1F80_1803.Index1`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum CdInterrupt {
    /// Resposta a um comando de leitura de dados.
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
    /// A bandeja está aberta.
    pub const SHELL_OPEN: u8 = 1 << 4;
    /// O motor está girando.
    pub const MOTOR_ON: u8 = 1 << 1;
}

/// Uma resposta agendada, entregue depois de `delay` ciclos.
#[derive(Debug, Clone)]
struct PendingResponse {
    delay: i64,
    interrupt: CdInterrupt,
    bytes: Vec<u8>,
}

/// O controlador de CD-ROM.
#[derive(Debug, Clone)]
pub struct CdRom {
    /// Bits 0..1 de `0x1F80_1800`, que selecionam o significado das portas.
    index: u8,
    parameters: VecDeque<u8>,
    response: VecDeque<u8>,
    data: VecDeque<u8>,
    /// Máscara de interrupções habilitadas (`0x1F80_1802.Index1`).
    interrupt_enable: u8,
    /// Interrupção pendente (`0x1F80_1803.Index1`), 0 = nenhuma.
    interrupt_flags: u8,
    pending: VecDeque<PendingResponse>,
    /// Status corrente reportado por `GetStat`.
    status: u8,
    /// Há uma imagem de disco carregada.
    has_disc: bool,
    /// Comandos recebidos sem implementação, para diagnóstico.
    unimplemented: u64,
    last_unimplemented: u8,
}

/// Latência típica de um acknowledge, em ciclos de CPU.
const ACKNOWLEDGE_DELAY: i64 = 20_000;

impl CdRom {
    pub fn new() -> Self {
        Self {
            index: 0,
            parameters: VecDeque::with_capacity(16),
            response: VecDeque::with_capacity(16),
            data: VecDeque::new(),
            interrupt_enable: 0,
            interrupt_flags: 0,
            pending: VecDeque::new(),
            // Sem disco: bandeja reportada como aberta, que é o que o BIOS
            // espera para cair no shell.
            status: status::SHELL_OPEN,
            has_disc: false,
            unimplemented: 0,
            last_unimplemented: 0,
        }
    }

    /// Informa ao controlador que há (ou não) uma imagem carregada.
    pub fn set_disc_present(&mut self, present: bool) {
        self.has_disc = present;
        self.status = if present {
            status::MOTOR_ON
        } else {
            status::SHELL_OPEN
        };
    }

    pub const fn unimplemented_commands(&self) -> u64 {
        self.unimplemented
    }

    pub const fn last_unimplemented_command(&self) -> u8 {
        self.last_unimplemented
    }

    /// Avança o controlador em `cycles` ciclos de CPU.
    pub fn step(&mut self, cycles: u32, irq: &mut IrqController) {
        // Só entrega a próxima resposta quando a anterior foi reconhecida.
        if self.interrupt_flags != 0 {
            return;
        }
        let Some(front) = self.pending.front_mut() else {
            return;
        };
        front.delay -= cycles as i64;
        if front.delay > 0 {
            return;
        }

        let ready = self.pending.pop_front().expect("front existe");
        self.response.clear();
        self.response.extend(ready.bytes);
        self.interrupt_flags = ready.interrupt as u8;

        if self.interrupt_enable & self.interrupt_flags != 0 {
            irq.raise(Interrupt::CdRom);
        }
    }

    /// Leitura de `0x1F80_1800..0x1F80_1803`.
    pub fn read(&mut self, offset: u32) -> u8 {
        match offset & 3 {
            0 => self.status_register(),
            1 => self.response.pop_front().unwrap_or(0),
            2 => self.data.pop_front().unwrap_or(0),
            _ => match self.index {
                0 | 2 => self.interrupt_enable | 0xE0,
                _ => self.interrupt_flags | 0xE0,
            },
        }
    }

    /// `0x1F80_1800` — índice e bits de prontidão.
    fn status_register(&self) -> u8 {
        let mut value = self.index;
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
        if !self.data.is_empty() {
            value |= 1 << 6;
        }
        value
    }

    /// Escrita em `0x1F80_1800..0x1F80_1803`.
    pub fn write(&mut self, offset: u32, value: u8) {
        match offset & 3 {
            0 => self.index = value & 3,
            1 => match self.index {
                0 => self.execute(value),
                _ => self.unhandled(value),
            },
            2 => match self.index {
                0 => {
                    if self.parameters.len() < 16 {
                        self.parameters.push_back(value);
                    }
                }
                1 => self.interrupt_enable = value & 0x1F,
                _ => self.unhandled(value),
            },
            _ => match self.index {
                1 => {
                    // Acknowledge: escrever 1 nos bits limpa as flags.
                    self.interrupt_flags &= !(value & 0x1F);
                    if value & 0x40 != 0 {
                        self.parameters.clear();
                    }
                }
                _ => self.unhandled(value),
            },
        }
    }

    fn unhandled(&mut self, value: u8) {
        self.unimplemented += 1;
        self.last_unimplemented = value;
    }

    fn schedule(&mut self, interrupt: CdInterrupt, bytes: Vec<u8>, delay: i64) {
        self.pending.push_back(PendingResponse {
            delay,
            interrupt,
            bytes,
        });
    }

    /// Executa um comando escrito em `0x1F80_1801.Index0`.
    fn execute(&mut self, command: u8) {
        let parameters: Vec<u8> = self.parameters.drain(..).collect();

        match command {
            // GetStat
            0x01 => self.schedule(
                CdInterrupt::Acknowledge,
                vec![self.status],
                ACKNOWLEDGE_DELAY,
            ),
            // Setmode
            0x0E => self.schedule(
                CdInterrupt::Acknowledge,
                vec![self.status],
                ACKNOWLEDGE_DELAY,
            ),
            // Mute / Demute
            0x0B | 0x0C => self.schedule(
                CdInterrupt::Acknowledge,
                vec![self.status],
                ACKNOWLEDGE_DELAY,
            ),
            // Init: responde duas vezes.
            0x0A => {
                self.status = if self.has_disc {
                    status::MOTOR_ON
                } else {
                    status::SHELL_OPEN
                };
                self.schedule(
                    CdInterrupt::Acknowledge,
                    vec![self.status],
                    ACKNOWLEDGE_DELAY,
                );
                self.schedule(
                    CdInterrupt::Complete,
                    vec![self.status],
                    ACKNOWLEDGE_DELAY * 5,
                );
            }
            // GetID: acknowledge seguido do identificador do disco.
            0x1A => {
                self.schedule(
                    CdInterrupt::Acknowledge,
                    vec![self.status],
                    ACKNOWLEDGE_DELAY,
                );
                if self.has_disc {
                    // Disco licenciado, região América. O agente `@cdrom` deve
                    // derivar isso da imagem em vez de fixar aqui.
                    self.schedule(
                        CdInterrupt::Complete,
                        vec![0x02, 0x00, 0x20, 0x00, b'S', b'C', b'E', b'A'],
                        ACKNOWLEDGE_DELAY * 3,
                    );
                } else {
                    // INT5 com "no disc".
                    self.schedule(
                        CdInterrupt::Error,
                        vec![0x08, 0x40, 0, 0, 0, 0, 0, 0],
                        ACKNOWLEDGE_DELAY * 3,
                    );
                }
            }
            // Test
            0x19 => match parameters.first() {
                // Versão do controlador (data de build do firmware).
                Some(0x20) => self.schedule(
                    CdInterrupt::Acknowledge,
                    vec![0x94, 0x09, 0x19, 0xC0],
                    ACKNOWLEDGE_DELAY,
                ),
                _ => self.schedule(
                    CdInterrupt::Acknowledge,
                    vec![self.status],
                    ACKNOWLEDGE_DELAY,
                ),
            },
            _ => {
                self.unimplemented += 1;
                self.last_unimplemented = command;
                // Responder com erro é melhor que travar: o BIOS trata INT5.
                self.schedule(
                    CdInterrupt::Error,
                    vec![self.status | 0x01, 0x40],
                    ACKNOWLEDGE_DELAY,
                );
            }
        }
    }
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
        let (mut cdrom, mut irq) = drive();
        cdrom.write(0, 1); // index 1
        cdrom.write(2, 0x1F); // habilita todas as IRQs
        cdrom.write(0, 0); // index 0
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
        let (mut cdrom, mut irq) = drive();
        cdrom.write(0, 1);
        cdrom.write(2, 0x1F);
        cdrom.write(0, 0);
        cdrom.write(1, 0x0A); // Init: duas respostas

        cdrom.step(ACKNOWLEDGE_DELAY as u32, &mut irq);
        cdrom.write(0, 1);
        assert_eq!(cdrom.read(3) & 0x1F, CdInterrupt::Acknowledge as u8);

        // Sem acknowledge, a segunda resposta não chega.
        cdrom.step(ACKNOWLEDGE_DELAY as u32 * 10, &mut irq);
        assert_eq!(cdrom.read(3) & 0x1F, CdInterrupt::Acknowledge as u8);

        // Acknowledge escrevendo 1 nos bits de flag.
        cdrom.write(3, 0x07);
        assert_eq!(cdrom.read(3) & 0x1F, 0);

        cdrom.step(ACKNOWLEDGE_DELAY as u32 * 10, &mut irq);
        assert_eq!(cdrom.read(3) & 0x1F, CdInterrupt::Complete as u8);
    }

    #[test]
    fn getid_without_disc_reports_an_error() {
        let (mut cdrom, mut irq) = drive();
        cdrom.write(0, 1);
        cdrom.write(2, 0x1F);
        cdrom.write(0, 0);
        cdrom.write(1, 0x1A); // GetID

        cdrom.step(ACKNOWLEDGE_DELAY as u32, &mut irq);
        cdrom.write(0, 1);
        assert_eq!(cdrom.read(3) & 0x1F, CdInterrupt::Acknowledge as u8);
        cdrom.write(3, 0x07);

        cdrom.step(ACKNOWLEDGE_DELAY as u32 * 5, &mut irq);
        assert_eq!(cdrom.read(3) & 0x1F, CdInterrupt::Error as u8);
        assert_eq!(cdrom.read(1), 0x08, "primeiro byte indica 'no disc'");
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
        let (mut cdrom, mut irq) = drive();
        cdrom.write(0, 1);
        cdrom.write(2, 0x1F);
        cdrom.write(0, 0);
        cdrom.write(2, 0x20); // parâmetro
        cdrom.write(1, 0x19); // Test

        cdrom.step(ACKNOWLEDGE_DELAY as u32, &mut irq);
        assert_eq!(cdrom.read(1), 0x94);
        assert_eq!(cdrom.read(1), 0x09);
    }

    #[test]
    fn unknown_command_is_counted_and_answered_with_an_error() {
        let (mut cdrom, mut irq) = drive();
        cdrom.write(0, 1);
        cdrom.write(2, 0x1F);
        cdrom.write(0, 0);
        cdrom.write(1, 0x7E);

        assert_eq!(cdrom.unimplemented_commands(), 1);
        assert_eq!(cdrom.last_unimplemented_command(), 0x7E);

        cdrom.step(ACKNOWLEDGE_DELAY as u32, &mut irq);
        cdrom.write(0, 1);
        assert_eq!(cdrom.read(3) & 0x1F, CdInterrupt::Error as u8);
    }

    #[test]
    fn disc_present_changes_the_reported_status() {
        let (mut cdrom, _) = drive();
        assert_ne!(cdrom.status & status::SHELL_OPEN, 0);
        cdrom.set_disc_present(true);
        assert_eq!(cdrom.status & status::SHELL_OPEN, 0);
        assert_ne!(cdrom.status & status::MOTOR_ON, 0);
    }
}
