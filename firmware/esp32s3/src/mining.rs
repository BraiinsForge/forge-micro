//! BM1362 chip-side wiring: hashboard reset pin and the two long-running
//! tasks the mmm backend needs.
//!
//! `serial_task` owns the UART halves and drives `mmm::serial::SERIAL` for
//! the lifetime of the firmware; `miner_task` runs the bring-up / teardown
//! state machine in `mmm::miner::run`. Neither ever returns under normal
//! operation.
//!
//! The ESP32-S3 runs two cores, but the MMM serial task runs on Core 0 (the
//! `PRO_CPU` embassy executor) alongside embassy-net. This mirrors the
//! stm32f407 single-core trade-off: the S3's 128-byte hardware FIFO absorbs
//! ETH-ISR jitter well enough for the interrupt+FIFO UART path. Running the
//! serial task on Core 1 (`APP_CPU`) would require a cross-core channel for
//! the ASIC session, which adds complexity without clear benefit at this
//! baud rate. If FIFO overruns are observed in practice, move `serial_task`
//! to the App CPU executor and add a cross-core pipe.

use defmt::{error, info, Debug2Format};
use embassy_time::{Duration, Timer};
use esp_hal::gpio::{Level, Output, OutputConfig};

use crate::psu::BoardPsu;
use crate::uart_baudrate::{MmmUartRx, MmmUartTx};

/// Drives the hashboard reset line through an esp-hal GPIO output.
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
/// returns on a fatal serial error; the task idles so the rest of the system
/// keeps running and a human can pull logs.
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

/// Construct a `BoardResetPin` from the GPIO4 peripheral taken from `hw`.
///
/// Starts low (hashboard held in reset) so the miner task controls the
/// release timing.
#[must_use]
pub fn make_reset_pin(gpio4: esp_hal::peripherals::GPIO4<'static>) -> BoardResetPin {
    let pin = Output::new(gpio4, Level::Low, OutputConfig::default());
    BoardResetPin::new(pin)
}
