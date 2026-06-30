//! Drives `ubos_mining::Miner` against the gpiocdev-backed reset pin
//! and the workspace-static `BmmPsu`.

use embassy_time::{Duration, Timer};
use log::{error, info};

use crate::gpiod::{GpiodResetPin, RESET_LINE};
use crate::psu::HostPsu;

#[embassy_executor::task]
pub async fn miner_task(psu: &'static HostPsu) -> ! {
    let reset_pin = match GpiodResetPin::open(RESET_LINE) {
        Ok(pin) => pin,
        Err(error) => {
            error!("host-runner: failed to open reset gpio: {error}; miner disabled");
            ubos_mining::miner::run_idle().await
        }
    };
    info!("host-runner: miner bringup");
    let mut miner = ubos_mining::Miner::new(psu, reset_pin, crate::BOARD);
    miner.run().await;
    info!("host-runner: miner exited; shutting down");
    Timer::after(Duration::from_secs(1)).await;
    std::process::exit(0);
}

#[embassy_executor::task]
pub async fn miner_idle_task() -> ! {
    ubos_mining::miner::run_idle().await
}
