//! BM1362 chip-side wiring: hashboard reset pin and the two long-running
//! tasks the mmm backend needs.
//!
//! `serial_task` owns the UART halves and drives `mmm::serial::SERIAL` for
//! the lifetime of the firmware; `miner_task` runs the bring-up / teardown
//! state machine in `mmm::miner::run`. Neither ever returns under normal
//! operation: there is no SIGINT on bare metal, so `STOP_MINING` is never
//! raised and `mmm::miner::run` parks inside its bring-up loop.
//!
//! The STM32F439 is single-core so `serial_task` runs on the same executor
//! as embassy-net. USART3 uses DMA so ETH-ISR jitter only races at the DMA
//! transfer level; the embassy-stm32 ring-buffered RX driver absorbs most
//! bursts. This is the same trade-off accepted on bmm-pico for Core 0 tasks.

use defmt::{error, info, Debug2Format};
use embassy_stm32::gpio::Output;
use embassy_time::{Duration, Timer};

use crate::psu::BoardPsu;
use crate::uart_baudrate::{MmmUartRx, MmmUartTx};

/// Drives the hashboard reset line through an embassy-stm32 GPIO.
pub struct BoardResetPin {
    pin: Output<'static>,
}

impl BoardResetPin {
    #[must_use]
    pub fn new(pin: Output<'static>) -> Self {
        Self { pin }
    }
}

impl mmm::reset::ResetPin for BoardResetPin {
    fn enter_reset(&mut self) {
        self.pin.set_low();
    }

    fn exit_reset(&mut self) {
        self.pin.set_high();
    }
}

/// Long park used after either task's main loop returns. The mmm actor only
/// returns on a fatal serial error; rather than panic and reset the whole
/// firmware, the task idles so the rest of the system keeps running and a
/// human can pull logs.
const PARK_INTERVAL: Duration = Duration::from_secs(60);

#[embassy_executor::task]
pub async fn serial_task(rx: MmmUartRx, tx: MmmUartTx) -> ! {
    match mmm::serial::SERIAL.run(rx, tx, &crate::ASIC_SESSION).await {
        Ok(()) => info!("mmm serial actor terminated cleanly"),
        Err(kind) => error!("mmm serial actor exited: {:?}", Debug2Format(&kind)),
    }
    loop {
        Timer::after(PARK_INTERVAL).await;
    }
}

#[embassy_executor::task]
pub async fn miner_task(psu: &'static BoardPsu, mut reset: BoardResetPin) -> ! {
    mmm::miner::run(psu, &mut reset).await;
    info!("mmm miner exited");
    loop {
        Timer::after(PARK_INTERVAL).await;
    }
}
