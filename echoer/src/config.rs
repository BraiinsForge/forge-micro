//! Board constants and peripheral extraction for the XIAO ESP32-S3 echoer.
//! `Board::take` consumes the esp-hal `Peripherals` and returns the timer
//! group and UART pins that main.rs needs.

use esp_hal::peripherals::{GPIO8, GPIO9, TIMG0, UART1};

/// Every peripheral singleton the board consumes, extracted in one place.
///
/// XIAO ESP32-S3 silk-to-GPIO mapping (from Seeed Studio wiki):
///   D9  = GPIO8 = UART TX  →  wired to PD9 (RX) on stm32f407/stm32f439
///   D10 = GPIO9 = UART RX  ←  wired to PD8 (TX) on stm32f407/stm32f439
pub struct Board<'d> {
    pub timg0: TIMG0<'d>,
    pub uart1: UART1<'d>,
    pub uart_tx: GPIO8<'d>,
    pub uart_rx: GPIO9<'d>,
}

impl Board<'static> {
    pub fn take(peripherals: esp_hal::peripherals::Peripherals) -> Self {
        Self {
            timg0: peripherals.TIMG0,
            uart1: peripherals.UART1,
            uart_tx: peripherals.GPIO8,
            uart_rx: peripherals.GPIO9,
        }
    }
}
