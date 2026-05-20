//! Brings up the ubos-miner BM1362 serial actor against a Linux tty.
//!
//! Opens `/dev/ttySTM1` at the chip's reset baud rate and runs the
//! actor's server loop on the split halves. Failure to open the device
//! parks the task instead of crashing the runner so the rest of the
//! tasks (network, stratum, metrics) keep working when no hashboard is
//! wired in.

use embassy_time::{Duration, Timer};
use embedded_serial_unix::Serial;
use log::{error, info};
use ubos_miner::consts::INITIAL_BAUDRATE;
use ubos_miner::serial::SERIAL;

use crate::ASIC_SESSION;

/// Path ubos-miner opens for chip-side traffic. Hard-coded because the BMM100
/// hashboard always presents the same device under our udev rules; if a
/// future host needs a different path, an env-var override would slot in
/// next to `HOST_NET_AFPACKET`.
const MINER_SERIAL_PATH: &str = "/dev/ttySTM1";

#[embassy_executor::task]
pub async fn miner_serial_task() -> ! {
    let serial = match Serial::open(MINER_SERIAL_PATH, INITIAL_BAUDRATE) {
        Ok(serial) => serial,
        Err(error) => {
            error!(
                "host-runner: failed to open {MINER_SERIAL_PATH}: {error}; miner serial disabled"
            );
            loop {
                Timer::after(Duration::from_secs(60)).await;
            }
        }
    };
    let (rx, tx) = serial.split();
    info!("host-runner: miner serial bringup on {MINER_SERIAL_PATH}");
    match SERIAL.run(rx, tx, &ASIC_SESSION).await {
        Ok(()) => info!("host-runner: miner serial actor terminated cleanly"),
        Err(kind) => error!("host-runner: miner serial actor exited: {kind:?}"),
    }
    // Park: the serial line is gone (EOF, IO error) or the actor was
    // told to stop. Re-opening would need fresh peripheral state which
    // the host doesn't have a clean trigger for here, so the rest of
    // the runner keeps running while this task idles.
    loop {
        Timer::after(Duration::from_secs(60)).await;
    }
}
