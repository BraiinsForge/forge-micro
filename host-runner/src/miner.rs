//! Drives `mmm::miner::run` against the gpiocdev-backed reset pin.

use embassy_time::{Duration, Timer};
use log::{error, info};

use crate::gpiod::{GpiodResetPin, RESET_LINE};

#[embassy_executor::task]
pub async fn miner_task() -> ! {
    let mut reset_pin = match GpiodResetPin::open(RESET_LINE) {
        Ok(pin) => pin,
        Err(error) => {
            error!("host-runner: failed to open reset gpio: {error}; miner disabled");
            loop {
                Timer::after(Duration::from_secs(60)).await;
            }
        }
    };
    info!("host-runner: miner bringup");
    mmm::miner::run(&mut reset_pin).await;
    info!("host-runner: miner exited");
    loop {
        Timer::after(Duration::from_secs(60)).await;
    }
}
