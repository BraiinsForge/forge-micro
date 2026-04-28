//! Brings up the mmm BM1362 serial actor against a Linux tty.
//!
//! Opens `/dev/ttySTM1` at the chip's reset baud rate and runs the
//! actor's server loop on the split halves. Failure to open the device
//! parks the task instead of crashing the runner so the rest of the
//! tasks (network, stratum, metrics) keep working when no hashboard is
//! wired in.

use embassy_time::{Duration, Timer};
use embedded_serial_unix::Serial;
use mmm::consts::INITIAL_BAUDRATE;
use mmm::serial::Serial as MmmSerial;

/// Path mmm opens for chip-side traffic. Hard-coded because the BMM100
/// hashboard always presents the same device under our udev rules; if a
/// future host needs a different path, an env-var override would slot in
/// next to `HOST_NET_AFPACKET`.
const MMM_SERIAL_PATH: &str = "/dev/ttySTM1";

/// Static actor instance. Exposed so future orchestration tasks can call
/// `start_sending_work`, `set_baud_rate`, etc. against the same handle.
pub static MMM_SERIAL: MmmSerial = MmmSerial::new();

#[embassy_executor::task]
pub async fn mmm_serial_task() -> ! {
    let serial = match Serial::open(MMM_SERIAL_PATH, INITIAL_BAUDRATE) {
        Ok(serial) => serial,
        Err(error) => {
            log::error!(
                "host-runner: failed to open {MMM_SERIAL_PATH}: {error}; mmm serial disabled"
            );
            loop {
                Timer::after(Duration::from_secs(60)).await;
            }
        }
    };
    let (rx, tx) = serial.split();
    log::info!("host-runner: mmm serial bringup on {MMM_SERIAL_PATH}");
    MMM_SERIAL.run(rx, tx).await
}
