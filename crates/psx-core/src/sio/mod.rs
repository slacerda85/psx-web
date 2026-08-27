//! SIO0 — controllers e memory cards.
//!
//! Referência: PSX-SPX — "Controllers and Memory Cards",
//! "Controller and Memory Card I/O Ports".
//!
//! **Escopo atual:** o handshake byte a byte de SIO0 e o protocolo do
//! controller **digital** (`0x5A41`) estão implementados, com IRQ7 no `ACK`.
//! Memory cards respondem "ausente", e o DualShock (`0x5A73`, modo analógico,
//! config mode e rumble) é entrega do agente `@sio`.

use crate::irq::{Interrupt, IrqController};

/// Botões do controller digital, na ordem dos bits do protocolo.
///
/// O protocolo é **ativo-baixo**: bit em 0 significa botão pressionado.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum Button {
    Select = 0,
    L3 = 1,
    R3 = 2,
    Start = 3,
    Up = 4,
    Right = 5,
    Down = 6,
    Left = 7,
    L2 = 8,
    R2 = 9,
    L1 = 10,
    R1 = 11,
    Triangle = 12,
    Circle = 13,
    Cross = 14,
    Square = 15,
}

/// Estado dos botões de um controller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ButtonState(u16);

impl ButtonState {
    /// Nenhum botão pressionado (todos os bits em 1).
    pub const RELEASED: ButtonState = ButtonState(0xFFFF);

    pub fn set(&mut self, button: Button, pressed: bool) {
        let bit = 1 << (button as u16);
        if pressed {
            self.0 &= !bit;
        } else {
            self.0 |= bit;
        }
    }

    pub const fn is_pressed(&self, button: Button) -> bool {
        self.0 & (1 << (button as u16)) == 0
    }

    /// Palavra crua no formato do protocolo (ativo-baixo).
    pub const fn raw(&self) -> u16 {
        self.0
    }

    /// Constrói a partir de um bitfield ativo-**alto**, que é o formato mais
    /// natural para o frontend enviar.
    pub const fn from_pressed_mask(mask: u16) -> Self {
        ButtonState(!mask)
    }
}

impl Default for ButtonState {
    fn default() -> Self {
        ButtonState::RELEASED
    }
}

/// Ciclos entre o byte e o `/ACK` do dispositivo (PSX-SPX — "Controller and
/// Memory Card Signals").
const ACK_DELAY: i32 = 338;

/// Alvo da transferência em andamento.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    None,
    Controller,
    MemoryCard,
}

/// O bloco SIO0.
#[derive(Debug, Clone)]
pub struct Sio {
    buttons: [ButtonState; 2],
    /// Controller conectado em cada slot.
    connected: [bool; 2],

    control: u16,
    mode: u16,
    baud: u16,

    /// Byte pronto para leitura em `JOY_DATA`.
    receive: u8,
    receive_full: bool,
    /// O dispositivo puxou a linha de `/ACK` (gera IRQ7).
    ack: bool,
    irq_raised: bool,
    /// Ciclos restantes até o dispositivo puxar `/ACK`. Zero = nada pendente.
    pending_ack: i32,

    target: Target,
    /// Índice do byte dentro da sequência de resposta do dispositivo.
    sequence: usize,

    /// Bytes de botão já entregues ao console, para diagnóstico.
    button_bytes_sent: u64,
    /// Transações iniciadas com o byte 0x01 (controller), para diagnóstico.
    controller_selects: u64,
    /// Bytes escritos em JOY_DATA, para diagnóstico.
    writes: u64,
}

impl Sio {
    pub fn new() -> Self {
        Self {
            buttons: [ButtonState::RELEASED; 2],
            connected: [true, false],
            control: 0,
            mode: 0,
            baud: 0,
            receive: 0xFF,
            receive_full: false,
            ack: false,
            irq_raised: false,
            pending_ack: 0,
            target: Target::None,
            sequence: 0,
            button_bytes_sent: 0,
            controller_selects: 0,
            writes: 0,
        }
    }

    /// Retrato do tráfego no SIO0, para diagnóstico.
    pub fn debug_state(&self) -> String {
        format!(
            "escritas={} selects={} bytes_de_botao={} control={:#06X} conectado={:?}",
            self.writes,
            self.controller_selects,
            self.button_bytes_sent,
            self.control,
            self.connected
        )
    }

    /// Atualiza o estado dos botões de um slot (0 ou 1).
    pub fn set_buttons(&mut self, slot: usize, state: ButtonState) {
        if slot < self.buttons.len() {
            self.buttons[slot] = state;
        }
    }

    /// Conecta ou desconecta o controller de um slot.
    pub fn set_connected(&mut self, slot: usize, connected: bool) {
        if slot < self.connected.len() {
            self.connected[slot] = connected;
        }
    }

    /// Slot selecionado por `JOY_CTRL.13`.
    const fn selected_slot(&self) -> usize {
        ((self.control >> 13) & 1) as usize
    }

    /// `JOY_CTRL.1` — chip select ligado.
    const fn is_selected(&self) -> bool {
        self.control & (1 << 1) != 0
    }

    /// Leitura de `0x1F80_1040..0x1F80_104F`.
    pub fn read(&mut self, offset: u32) -> u32 {
        match offset & 0x0F {
            0x00 => {
                let value = self.receive;
                self.receive = 0xFF;
                self.receive_full = false;
                value as u32
            }
            0x04 => self.status(),
            0x08 => self.mode as u32,
            0x0A => self.control as u32,
            0x0E => self.baud as u32,
            _ => 0,
        }
    }

    /// `JOY_STAT` (`0x1F80_1044`).
    fn status(&self) -> u32 {
        let mut status = 0u32;
        // Bit 0: TX pronto (sempre, já que a transferência é instantânea).
        status |= 1;
        // Bit 1: há byte para ler em JOY_DATA.
        status |= (self.receive_full as u32) << 1;
        // Bit 2: TX terminado.
        status |= 1 << 2;
        // Bit 7: nível de ACK (ativo-baixo no hardware).
        status |= (self.ack as u32) << 7;
        // Bit 9: IRQ pendente.
        status |= (self.irq_raised as u32) << 9;
        status
    }

    /// Escrita em `0x1F80_1040..0x1F80_104F`.
    pub fn write(&mut self, offset: u32, value: u32) {
        match offset & 0x0F {
            0x00 => self.transfer(value as u8),
            0x08 => self.mode = value as u16,
            0x0A => self.write_control(value as u16),
            0x0E => self.baud = value as u16,
            _ => {}
        }
    }

    fn write_control(&mut self, value: u16) {
        self.control = value;

        // Bit 4: acknowledge da IRQ.
        //
        // Reconhece a **interrupção**, e só ela. A linha `/ACK` é uma entrada
        // vinda do dispositivo: o console não a controla. Derrubá-la aqui
        // apagava a única prova de que havia um controller no slot — o BIOS
        // escreve este bit entre enviar o byte e ler o status, então lia o
        // status já sem o `/ACK` e concluía que o slot estava vazio.
        if value & (1 << 4) != 0 {
            self.irq_raised = false;
        }
        // Bit 6: reset completo.
        if value & (1 << 6) != 0 {
            self.control = 0;
            self.mode = 0;
            self.receive = 0xFF;
            self.receive_full = false;
            self.irq_raised = false;
            self.ack = false;
            self.target = Target::None;
            self.sequence = 0;
        }
        // Soltar o chip select encerra a transação.
        if !self.is_selected() {
            self.target = Target::None;
            self.sequence = 0;
        }
    }

    /// Um byte enviado pela CPU em `JOY_DATA`; devolve o byte simultâneo do
    /// dispositivo e, se houver, o `ACK` que gera IRQ7.
    fn transfer(&mut self, sent: u8) {
        self.writes += 1;
        self.transfer_inner(sent);
    }

    fn transfer_inner(&mut self, sent: u8) {
        // Um byte novo derruba o /ACK do byte anterior.
        self.ack = false;
        if !self.is_selected() {
            self.receive = 0xFF;
            self.receive_full = true;
            self.ack = false;
            return;
        }

        let slot = self.selected_slot();

        // O primeiro byte da transação escolhe o dispositivo:
        // 0x01 = controller, 0x81 = memory card.
        if self.target == Target::None {
            self.target = match sent {
                0x01 if self.connected[slot] => {
                    self.controller_selects += 1;
                    Target::Controller
                }
                0x81 => Target::MemoryCard,
                _ => Target::None,
            };
            self.sequence = 0;

            // Sem dispositivo: linha alta e nenhum ACK, que é como o BIOS
            // detecta um slot vazio.
            if self.target == Target::None {
                self.receive = 0xFF;
                self.receive_full = true;
                self.ack = false;
                return;
            }

            self.receive = 0xFF;
            self.receive_full = true;
            self.schedule_ack();
            return;
        }

        let response = match self.target {
            Target::Controller => {
                if self.sequence == 2 || self.sequence == 3 {
                    self.button_bytes_sent += 1;
                }
                self.controller_byte(slot, self.sequence)
            }
            // TODO(@sio): implementar o protocolo do memory card. Responder
            // 0xFF sem ACK faz o BIOS concluir "sem cartão".
            Target::MemoryCard | Target::None => None,
        };

        self.sequence += 1;

        match response {
            Some((byte, more)) => {
                self.receive = byte;
                self.receive_full = true;
                if more {
                    self.schedule_ack();
                } else {
                    self.ack = false;
                    self.target = Target::None;
                    self.sequence = 0;
                }
            }
            None => {
                self.receive = 0xFF;
                self.receive_full = true;
                self.ack = false;
                self.target = Target::None;
                self.sequence = 0;
            }
        }
    }

    /// Sequência de resposta do controller digital.
    ///
    /// Depois do `0x01` inicial, o dispositivo responde
    /// `0x41 0x5A btn_lo btn_hi` — o ID `0x5A41` seguido dos dois bytes de
    /// botões, ativo-baixo.
    fn controller_byte(&self, slot: usize, index: usize) -> Option<(u8, bool)> {
        let buttons = self.buttons[slot].raw();
        match index {
            0 => Some((0x41, true)),
            1 => Some((0x5A, true)),
            2 => Some((buttons as u8, true)),
            // Último byte: sem ACK, encerra a transação.
            3 => Some(((buttons >> 8) as u8, false)),
            _ => None,
        }
    }

    /// Agenda o `/ACK` do dispositivo.
    ///
    /// O `/ACK` **não** é simultâneo ao byte: o controller responde algumas
    /// centenas de ciclos depois. Disparar na hora quebra o BIOS, que escreve
    /// o acknowledge de `JOY_CTRL` logo após enviar o byte e só então vai
    /// esperar a IRQ — com o ACK imediato, ele apagava a própria interrupção
    /// que estava prestes a aguardar, dava timeout e concluía "slot vazio".
    fn schedule_ack(&mut self) {
        self.pending_ack = ACK_DELAY;
    }

    /// Avança o `/ACK` pendente. Chamado com os ciclos gastos pela CPU.
    pub fn step(&mut self, cycles: u32, irq: &mut IrqController) {
        if self.pending_ack <= 0 {
            return;
        }
        self.pending_ack -= cycles as i32;
        if self.pending_ack > 0 {
            return;
        }
        self.pending_ack = 0;
        self.ack = true;
        // Bit 12 de JOY_CTRL habilita a IRQ no ACK.
        if self.control & (1 << 12) != 0 {
            self.irq_raised = true;
            irq.raise(Interrupt::ControllerAndMemoryCard);
        }
    }
}

impl Default for Sio {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `JOY_CTRL` com chip select ligado, IRQ no ACK habilitada, slot 0.
    const SELECT_SLOT0: u32 = (1 << 1) | (1 << 12);

    fn sio() -> (Sio, IrqController) {
        let mut irq = IrqController::new();
        irq.write_mask(0x07FF);
        (Sio::new(), irq)
    }

    /// Envia um byte, deixa o `/ACK` chegar e devolve a resposta.
    ///
    /// O `/ACK` não é simultâneo ao byte, então um teste que não avança o
    /// tempo nunca o veria — que era exatamente o que mascarava o bug.
    fn exchange(sio: &mut Sio, irq: &mut IrqController, byte: u8) -> u8 {
        sio.write(0x00, byte as u32);
        let response = sio.read(0x00) as u8;
        sio.step(ACK_DELAY as u32, irq);
        response
    }

    #[test]
    fn digital_controller_reports_its_id_and_buttons() {
        let (mut sio, mut irq) = sio();
        sio.write(0x0A, SELECT_SLOT0);

        assert_eq!(exchange(&mut sio, &mut irq, 0x01), 0xFF, "byte de seleção");
        assert_eq!(exchange(&mut sio, &mut irq, 0x42), 0x41, "ID baixo");
        assert_eq!(exchange(&mut sio, &mut irq, 0x00), 0x5A, "ID alto");
        assert_eq!(exchange(&mut sio, &mut irq, 0x00), 0xFF, "nenhum botão");
        assert_eq!(exchange(&mut sio, &mut irq, 0x00), 0xFF);
    }

    #[test]
    fn pressed_buttons_clear_their_bit() {
        let (mut sio, mut irq) = sio();
        let mut state = ButtonState::RELEASED;
        state.set(Button::Cross, true);
        state.set(Button::Start, true);
        sio.set_buttons(0, state);

        sio.write(0x0A, SELECT_SLOT0);
        exchange(&mut sio, &mut irq, 0x01);
        exchange(&mut sio, &mut irq, 0x42);
        exchange(&mut sio, &mut irq, 0x00);
        let low = exchange(&mut sio, &mut irq, 0x00);
        let high = exchange(&mut sio, &mut irq, 0x00);

        // Start é o bit 3 do byte baixo, Cross o bit 6 do byte alto.
        assert_eq!(low & (1 << 3), 0, "Start pressionado");
        assert_eq!(high & (1 << 6), 0, "Cross pressionado");
        assert_ne!(low & (1 << 0), 0, "Select solto");
    }

    #[test]
    fn ack_raises_irq7_while_more_bytes_remain() {
        let (mut sio, mut irq) = sio();
        sio.write(0x0A, SELECT_SLOT0);

        exchange(&mut sio, &mut irq, 0x01);
        assert_ne!(
            irq.stat() & (1 << Interrupt::ControllerAndMemoryCard as u16),
            0,
            "ACK do byte de seleção gera IRQ7"
        );
    }

    #[test]
    fn last_byte_does_not_acknowledge() {
        let (mut sio, mut irq) = sio();
        sio.write(0x0A, SELECT_SLOT0);

        for byte in [0x01, 0x42, 0x00, 0x00] {
            exchange(&mut sio, &mut irq, byte);
        }
        // Quarto byte da resposta (botões altos) encerra sem ACK.
        exchange(&mut sio, &mut irq, 0x00);
        assert_eq!(sio.status() & (1 << 7), 0, "ACK baixo no fim da transação");
    }

    #[test]
    fn empty_slot_answers_all_ones_without_ack() {
        let (mut sio, mut irq) = sio();
        sio.set_connected(0, false);
        sio.write(0x0A, SELECT_SLOT0);

        assert_eq!(exchange(&mut sio, &mut irq, 0x01), 0xFF);
        assert_eq!(sio.status() & (1 << 7), 0, "sem ACK");
        assert_eq!(
            irq.stat() & (1 << Interrupt::ControllerAndMemoryCard as u16),
            0
        );
    }

    #[test]
    fn memory_card_reports_absent() {
        let (mut sio, mut irq) = sio();
        sio.write(0x0A, SELECT_SLOT0);
        exchange(&mut sio, &mut irq, 0x81);
        assert_eq!(exchange(&mut sio, &mut irq, 0x52), 0xFF, "sem cartão");
    }

    #[test]
    fn transfer_without_chip_select_is_ignored() {
        let (mut sio, mut irq) = sio();
        // Sem escrever JOY_CTRL, o chip select está desligado.
        assert_eq!(exchange(&mut sio, &mut irq, 0x01), 0xFF);
        assert_eq!(sio.status() & (1 << 7), 0);
    }

    #[test]
    fn acknowledge_bit_clears_the_pending_irq() {
        let (mut sio, mut irq) = sio();
        sio.write(0x0A, SELECT_SLOT0);
        exchange(&mut sio, &mut irq, 0x01);
        assert_ne!(sio.status() & (1 << 9), 0, "IRQ marcada em JOY_STAT");

        sio.write(0x0A, SELECT_SLOT0 | (1 << 4));
        assert_eq!(sio.status() & (1 << 9), 0);
    }

    #[test]
    fn slot_one_has_its_own_button_state() {
        let (mut sio, mut irq) = sio();
        sio.set_connected(1, true);
        let mut state = ButtonState::RELEASED;
        state.set(Button::Square, true);
        sio.set_buttons(1, state);

        // Bit 13 de JOY_CTRL seleciona o slot 2.
        sio.write(0x0A, SELECT_SLOT0 | (1 << 13));
        exchange(&mut sio, &mut irq, 0x01);
        exchange(&mut sio, &mut irq, 0x42);
        exchange(&mut sio, &mut irq, 0x00);
        exchange(&mut sio, &mut irq, 0x00);
        let high = exchange(&mut sio, &mut irq, 0x00);
        assert_eq!(high & (1 << 7), 0, "Square pressionado no slot 2");
    }

    #[test]
    fn pressed_mask_helper_inverts_correctly() {
        let state = ButtonState::from_pressed_mask(1 << Button::Cross as u16);
        assert!(state.is_pressed(Button::Cross));
        assert!(!state.is_pressed(Button::Circle));
    }

    #[test]
    fn the_ack_arrives_after_the_byte_and_not_with_it() {
        let (mut sio, mut irq) = sio();
        sio.write(0x0A, SELECT_SLOT0);

        sio.write(0x00, 0x01);
        assert_eq!(
            sio.status() & (1 << 7),
            0,
            "o /ACK não é simultâneo ao byte"
        );
        assert_eq!(
            irq.stat() & (1 << Interrupt::ControllerAndMemoryCard as u16),
            0,
            "e a IRQ também não"
        );

        // Antes do prazo, ainda nada.
        sio.step(ACK_DELAY as u32 / 2, &mut irq);
        assert_eq!(sio.status() & (1 << 7), 0);

        sio.step(ACK_DELAY as u32, &mut irq);
        assert_ne!(sio.status() & (1 << 7), 0, "o /ACK chega depois do atraso");
        assert_ne!(
            irq.stat() & (1 << Interrupt::ControllerAndMemoryCard as u16),
            0,
            "e leva a IRQ7 junto"
        );
    }

    #[test]
    fn acknowledging_the_irq_does_not_drop_the_ack_line() {
        let (mut sio, mut irq) = sio();
        sio.write(0x0A, SELECT_SLOT0);
        sio.write(0x00, 0x01);
        sio.step(ACK_DELAY as u32, &mut irq);
        assert_ne!(sio.status() & (1 << 7), 0);

        // Bit 4 reconhece a interrupção. O BIOS escreve isto entre enviar o
        // byte e ler o status; se derrubasse o /ACK junto, ele leria o status
        // sem a prova de que há um controller e desistiria do slot.
        sio.write(0x0A, SELECT_SLOT0 | (1 << 4));

        assert_eq!(sio.status() & (1 << 9), 0, "a IRQ foi reconhecida");
        assert_ne!(sio.status() & (1 << 7), 0, "o /ACK continua de pé");
    }

    #[test]
    fn a_new_byte_drops_the_previous_ack() {
        let (mut sio, mut irq) = sio();
        sio.write(0x0A, SELECT_SLOT0);
        sio.write(0x00, 0x01);
        sio.step(ACK_DELAY as u32, &mut irq);
        assert_ne!(sio.status() & (1 << 7), 0);

        sio.write(0x00, 0x42);
        assert_eq!(sio.status() & (1 << 7), 0, "o /ACK anterior caiu");
    }

    #[test]
    fn a_full_pad_read_delivers_both_button_bytes() {
        let (mut sio, mut irq) = sio();
        let mut state = ButtonState::RELEASED;
        state.set(Button::Down, true);
        sio.set_buttons(0, state);
        sio.write(0x0A, SELECT_SLOT0);

        // A sequência completa que o BIOS emite: 01 42 00 00 00.
        let mut responses = Vec::new();
        for byte in [0x01u8, 0x42, 0x00, 0x00, 0x00] {
            responses.push(exchange(&mut sio, &mut irq, byte));
        }

        assert_eq!(responses[0], 0xFF, "seleção");
        assert_eq!(responses[1], 0x41, "ID baixo");
        assert_eq!(responses[2], 0x5A, "ID alto");
        // Down é o bit 6 do byte baixo, ativo-baixo.
        assert_eq!(responses[3] & (1 << 6), 0, "Down pressionado");
        assert_eq!(responses[4], 0xFF, "nada no byte alto");
    }
}
