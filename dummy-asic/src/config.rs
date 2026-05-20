//! Board constants and peripheral extraction for the XIAO ESP32-S3 dummy ASIC.
//!
//! Dummy ASIC: `../../docs/DUMMY_ASIC.md`.

use esp_hal::peripherals::{CPU_CTRL, GPIO8, GPIO9, SHA, SW_INTERRUPT, TIMG0, UART1};

/// Every peripheral singleton the board consumes, extracted in one place.
///
/// XIAO ESP32-S3 silk-to-GPIO mapping (from Seeed Studio wiki):
///   D9  = GPIO8 = UART TX  →  host RX
///   D10 = GPIO9 = UART RX  ←  host TX
pub struct Board<'d> {
    /// Timer group 0, used as the esp-rtos time base.
    pub timg0: TIMG0<'d>,
    /// Software interrupt control, needed for dual-core SMP scheduler.
    pub sw_interrupt: SW_INTERRUPT<'d>,
    /// CPU control peripheral for starting the `APP_CPU`.
    pub cpu_ctrl: CPU_CTRL<'d>,
    /// UART1 peripheral.
    pub uart1: UART1<'d>,
    /// GPIO8 — UART TX (XIAO silk D9).
    pub uart_tx: GPIO8<'d>,
    /// GPIO9 — UART RX (XIAO silk D10).
    pub uart_rx: GPIO9<'d>,
    /// SHA peripheral, used by the solver for hardware-accelerated SHA-256d.
    pub sha: SHA<'d>,
}

impl Board<'static> {
    /// Consume the HAL peripherals and return the board resources.
    pub fn take(peripherals: esp_hal::peripherals::Peripherals) -> Self {
        Self {
            timg0: peripherals.TIMG0,
            sw_interrupt: peripherals.SW_INTERRUPT,
            cpu_ctrl: peripherals.CPU_CTRL,
            uart1: peripherals.UART1,
            uart_tx: peripherals.GPIO8,
            uart_rx: peripherals.GPIO9,
            sha: peripherals.SHA,
        }
    }
}
