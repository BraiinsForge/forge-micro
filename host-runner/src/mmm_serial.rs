//! Brings up the mmm BM1362 serial actor against a Linux tty.
//!
//! Opens `/dev/ttySTM1` at the chip's reset baud rate and runs the
//! actor's server loop on the split halves. Failure to open the device
//! parks the task instead of crashing the runner so the rest of the
//! tasks (network, stratum, metrics) keep working when no hashboard is
//! wired in.

use embassy_time::{Duration, Timer};
use embedded_serial_unix::Serial;
use log::{error, info};
use mmm::consts::INITIAL_BAUDRATE;
use mmm::serial::SERIAL;
use stratum_client::AsicSession;

/// The shared work/solution rendezvous between the stratum-client
/// task and the BM1362 actor. Placeholder for the host-runner →
/// stratum-client wiring that lands in the next commit; for now
/// the actor reads from it but nobody publishes, so it idles
/// indefinitely waiting for the first assignment.
pub static ASIC_SESSION: AsicSession = AsicSession::new();

/// Path mmm opens for chip-side traffic. Hard-coded because the BMM100
/// hashboard always presents the same device under our udev rules; if a
/// future host needs a different path, an env-var override would slot in
/// next to `HOST_NET_AFPACKET`.
const MMM_SERIAL_PATH: &str = "/dev/ttySTM1";

#[embassy_executor::task]
pub async fn mmm_serial_task() -> ! {
    let serial = match Serial::open(MMM_SERIAL_PATH, INITIAL_BAUDRATE) {
        Ok(serial) => serial,
        Err(error) => {
            error!("host-runner: failed to open {MMM_SERIAL_PATH}: {error}; mmm serial disabled");
            loop {
                Timer::after(Duration::from_secs(60)).await;
            }
        }
    };
    let (rx, tx) = serial.split();
    info!("host-runner: mmm serial bringup on {MMM_SERIAL_PATH}");
    match SERIAL.run(rx, tx, &ASIC_SESSION).await {
        Ok(()) => info!("host-runner: mmm serial actor terminated cleanly"),
        Err(kind) => error!("host-runner: mmm serial actor exited: {kind:?}"),
    }
    // Park: the serial line is gone (EOF, IO error) or the actor was
    // told to stop. Re-opening would need fresh peripheral state which
    // the host doesn't have a clean trigger for here, so the rest of
    // the runner keeps running while this task idles.
    loop {
        Timer::after(Duration::from_secs(60)).await;
    }
}
