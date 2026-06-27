//! BM1362 chip-side wiring: hashboard reset pin and the two long-running
//! tasks the ubos-mining backend needs.
//!
//! `serial_task` owns the UART halves and drives `ubos_mining::serial::SERIAL` for
//! the lifetime of the firmware; `miner_task` runs the bring-up / teardown
//! state machine in `ubos_mining::Miner::run`. Neither ever returns under normal
//! operation.
//!
//! The ubos-mining `serial_task` runs on Core 1 (the `APP_CPU` embassy executor)
//! while embassy-net and the mining state machine run on Core 0, mirroring
//! the rp2350/bmm-pico dual-core split. `ASIC_SESSION` uses a
//! `CriticalSectionRawMutex`, so the cross-core share needs no extra pipe.

use defmt::{Debug2Format, error, info};
use embassy_time::{Duration, Timer};
use esp_hal::gpio::{Level, Output, OutputConfig};

use crate::psu::BoardPsu;
use crate::uart_baudrate::{MinerUartRx, MinerUartTx};

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

impl ubos_mining::reset::ResetPin for BoardResetPin {
    fn enter_reset(&mut self) {
        self.pin.set_low();
    }

    fn exit_reset(&mut self) {
        self.pin.set_high();
    }
}

/// Long park used after either task's main loop returns. The ubos-mining actor
/// only returns on a fatal serial error; the task idles so the rest of the
/// system keeps running and a human can pull logs.
const PARK_INTERVAL: Duration = Duration::from_secs(60);

#[embassy_executor::task]
pub async fn serial_task(rx: MinerUartRx, tx: MinerUartTx) -> ! {
    match ubos_mining::serial::SERIAL
        .run(rx, tx, &crate::ASIC_SESSION)
        .await
    {
        Ok(()) => info!("ubos-mining serial actor terminated cleanly"),
        Err(kind) => error!("ubos-mining serial actor exited: {:?}", Debug2Format(&kind)),
    }
    loop {
        Timer::after(PARK_INTERVAL).await;
    }
}

#[embassy_executor::task]
pub async fn miner_task(psu: &'static BoardPsu, reset: BoardResetPin) -> ! {
    let mut miner = ubos_mining::Miner::new(psu, reset);
    miner.run().await;
    info!("ubos-mining miner exited");
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
