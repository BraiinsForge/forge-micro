//! Hashboard reset pin plus the two long-running tasks the ubos-mining backend
//! spawns: the chip-serial actor and the bring-up/teardown state machine.
//!
//! Backend architecture: `../../../docs/MINING.md`.

use defmt::{error, info, Debug2Format};
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
pub async fn miner_task(psu: &'static BoardPsu, mut reset: BoardResetPin) -> ! {
    ubos_mining::miner::run(psu, &mut reset).await;
    info!("ubos-mining miner exited");
    loop {
        Timer::after(PARK_INTERVAL).await;
    }
}
