//! BMM100 PSU integration over the shared I2C0 bus.
//!
//! Owns the static cells for the `MiniminerPsu` state, the terminated flag,
//! and the PSU handle. The bus mutex is owned by `main` so future I2C
//! consumers can take additional `I2cDevice` handles from the same mutex
//! without going through this module.

use embassy_embedded_hal::shared_bus::asynch::i2c::I2cDevice;
use embassy_sync::blocking_mutex::raw::{CriticalSectionRawMutex, NoopRawMutex};
use embassy_sync::mutex::Mutex;
use esp_hal::i2c::master::I2c;
use portable_atomic::AtomicBool;
use static_cell::StaticCell;
use ubos_psu::{MiniminerPsu, MiniminerPsuConfig, State, run_watchdog};

/// Bus mutex shared by every consumer on I2C0. `NoopRawMutex` is sufficient
/// because every I2C consumer runs on the Core 0 executor. The chip-serial
/// actor (`mining::serial_task`) runs on Core 1 but does not touch I2C, so
/// there is no cross-core contention. If a future I2C consumer moves to
/// Core 1, bump this to `CriticalSectionRawMutex`.
pub type Bus = Mutex<NoopRawMutex, I2c<'static, esp_hal::Async>>;

/// `embedded-hal-async` device handle for one consumer of the shared bus.
type Device = I2cDevice<'static, NoopRawMutex, I2c<'static, esp_hal::Async>>;

/// PSU state guarded by the raw mutex `MiniminerPsu` requires.
type PsuState = Mutex<CriticalSectionRawMutex, State<Device>>;

/// Concrete `MiniminerPsu` instantiation for the BMM Adapter. Public so the
/// `mining` module can spell it in `miner_task`'s signature.
pub type BoardPsu = MiniminerPsu<Device>;

static STATE: StaticCell<PsuState> = StaticCell::new();
static TERMINATED: StaticCell<AtomicBool> = StaticCell::new();
static PSU: StaticCell<BoardPsu> = StaticCell::new();

/// Construct the `MiniminerPsu` over an `I2cDevice` carved from `bus` and
/// return the `'static` references the watchdog / miner / sensor tasks need.
pub fn init(bus: &'static Bus) -> (&'static BoardPsu, &'static PsuState, &'static AtomicBool) {
    let device = I2cDevice::new(bus);
    let state: &'static _ = STATE.init(Mutex::new(State::new(device, MiniminerPsuConfig::DEFAULT)));
    let terminated: &'static _ = TERMINATED.init(AtomicBool::new(false));
    let psu: &'static _ = PSU.init(MiniminerPsu::new(state, terminated));
    (psu, state, terminated)
}

#[embassy_executor::task]
pub async fn watchdog_task(state: &'static PsuState, terminated: &'static AtomicBool) {
    run_watchdog(state, terminated).await;
}

#[embassy_executor::task]
pub async fn power_stats_task(psu: &'static BoardPsu) -> ! {
    ubos_psu::run_power_stats(psu).await
}
