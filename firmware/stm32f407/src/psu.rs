//! BMM100 PSU integration over the shared I2C1 bus.
//!
//! Owns the static cells for the `MiniminerPsu` state, the terminated flag,
//! and the PSU handle. The bus mutex is owned by `main` so future I2C
//! consumers can take additional `I2cDevice` handles from the same mutex
//! without going through this module.

use embassy_embedded_hal::shared_bus::asynch::i2c::I2cDevice;
use embassy_stm32::i2c::{I2c, Master};
use embassy_sync::blocking_mutex::raw::{CriticalSectionRawMutex, NoopRawMutex};
use embassy_sync::mutex::Mutex;
use portable_atomic::AtomicBool;
use static_cell::StaticCell;
use ubos_psu::{MiniminerPsu, MiniminerPsuConfig, State, run_watchdog};

/// Bus mutex shared by every consumer on I2C1. `NoopRawMutex` is sufficient
/// because the STM32F407 is single-core: every I2C consumer runs on the same
/// executor and there is no cross-core contention.
pub type Bus = Mutex<NoopRawMutex, I2c<'static, embassy_stm32::mode::Async, Master>>;

/// `embedded-hal-async` device handle for one consumer of the shared bus.
type Device = I2cDevice<'static, NoopRawMutex, I2c<'static, embassy_stm32::mode::Async, Master>>;

/// PSU state guarded by the raw mutex `ubos_psu::MiniminerPsu` requires.
type PsuState = Mutex<CriticalSectionRawMutex, State<Device, MiniminerPsuConfig>>;
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
