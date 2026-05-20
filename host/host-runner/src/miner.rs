//! Drives `ubos_miner::miner::run` against the gpiocdev-backed reset pin
//! and the workspace-static `MiniminerPsu`.

use embassy_time::{Duration, Timer};
use log::{error, info};

use crate::gpiod::{GpiodResetPin, RESET_LINE};
use crate::psu::HostPsu;

#[embassy_executor::task]
pub async fn miner_task(psu: &'static HostPsu) -> ! {
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
    ubos_miner::miner::run(psu, &mut reset_pin).await;
    info!("host-runner: miner exited; shutting down");
    Timer::after(Duration::from_secs(1)).await;
    std::process::exit(0);
}
