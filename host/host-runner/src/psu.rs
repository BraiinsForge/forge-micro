//! Workspace-static `BmmPsu` over the Linux i2c-dev bus.
//!
//! Activated by `UBOS_I2C_BUS=/dev/i2c-N`; without the env var set
//! `open()` returns `None` and the runner skips the miner spawn. The
//! `'static` references are exposed so `main.rs` can hand them to the
//! watchdog and miner tasks.

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embedded_i2c_unix::I2c as HostI2c;
use log::{error, info};
use portable_atomic::AtomicBool;
use static_cell::StaticCell;
use ubos_psu::{BmmPsu, BmmPsuConfig, State, run_power_stats, run_watchdog};

/// Env var that points at the i2c-dev character device wired to the
/// BMM100 PSU. Unset means no hashboard is attached and the runner
/// skips the miner.
pub const I2C_BUS_ENV: &str = "UBOS_I2C_BUS";

pub type HostPsu = BmmPsu<HostI2c>;
pub type HostState = Mutex<CriticalSectionRawMutex, State<HostI2c, BmmPsuConfig>>;

static STATE: StaticCell<HostState> = StaticCell::new();
static TERMINATED: StaticCell<AtomicBool> = StaticCell::new();
static PSU: StaticCell<HostPsu> = StaticCell::new();

/// Open the I2C bus from `UBOS_I2C_BUS`, stand up the PSU + watchdog
/// state, and return the `'static` references the miner / watchdog
/// tasks need. Returns `None` if the env var is unset/empty or the
/// bus cannot be opened.
pub fn open() -> Option<(&'static HostPsu, &'static HostState, &'static AtomicBool)> {
    let env_path = std::env::var(I2C_BUS_ENV);
    let path = env_path.as_deref().unwrap_or("/dev/i2c-0");
    if path.is_empty() {
        return None;
    }
    let i2c = match HostI2c::open(path) {
        Ok(i2c) => i2c,
        Err(error) => {
            error!("psu: cannot open {path}: {error}");
            return None;
        }
    };
    info!("psu: opened {path}");
    let state: &'static _ = STATE.init(Mutex::new(State::new(i2c, BmmPsuConfig::DEFAULT)));
    let terminated: &'static _ = TERMINATED.init(AtomicBool::new(false));
    let psu: &'static _ = PSU.init(BmmPsu::new(state, terminated));
    Some((psu, state, terminated))
}

#[embassy_executor::task]
pub async fn watchdog_task(state: &'static HostState, terminated: &'static AtomicBool) {
    run_watchdog(state, terminated).await;
}

#[embassy_executor::task]
pub async fn power_stats_task(psu: &'static HostPsu) -> ! {
    run_power_stats(psu).await
}
