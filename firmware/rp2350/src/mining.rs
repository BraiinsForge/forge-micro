//! BM1362 chip-side wiring: hashboard reset pin and the two long-running
//! tasks the ubos-mining backend needs.
//!
//! `serial_task` owns the UART halves and drives `ubos_mining::serial::SERIAL` for
//! the lifetime of the firmware; `miner_task` runs the bring-up / teardown
//! state machine in `ubos_mining::Miner::run`. Neither ever returns under normal
//! operation: there is no SIGINT on bare metal, so `STOP_MINING` is never
//! raised and `ubos_mining::Miner::run` parks inside its bring-up loop.

use defmt::{Debug2Format, error, info};
use embassy_rp::gpio::Output;
use embassy_time::{Duration, Timer};

use crate::psu::BoardPsu;
use crate::uart_baudrate::{MinerUartRx, MinerUartTx};

/// Drives the hashboard reset line through an embassy-rp GPIO.
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
/// only returns on a fatal serial error; rather than panic and reset the whole
/// firmware, the task idles so the rest of the system keeps running and a
/// human can pull logs.
const PARK_INTERVAL: Duration = Duration::from_secs(60);

#[embassy_executor::task]
pub async fn serial_task(rx: MinerUartRx, tx: MinerUartTx) -> ! {
    match ubos_mining::serial::SERIAL
        .run(rx, tx, &ubos_firmware::ASIC_SESSION)
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
