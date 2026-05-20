//! Workspace-static `MiniminerPsu` over the Linux i2c-dev bus.
//!
//! Activated by `MMM_I2C_BUS=/dev/i2c-N`; without the env var set
//! `open()` returns `None` and the runner skips the miner spawn. The
//! `'static` references are exposed so `main.rs` can hand them to the
//! watchdog and miner tasks.

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_time::{Duration, Ticker};
use embedded_i2c_unix::{Error as HostI2cError, I2c as HostI2c};
use log::{error, info};
use portable_atomic::AtomicBool;
use static_cell::StaticCell;
use ubos_psu::{run_watchdog, MiniminerPsu, MiniminerPsuConfig, Psu, State};

/// Env var that points at the i2c-dev character device wired to the
/// BMM100 PSU. Unset means no hashboard is attached and the runner
/// skips the miner.
pub const I2C_BUS_ENV: &str = "MMM_I2C_BUS";

pub type HostPsu = MiniminerPsu<HostI2c, HostI2cError>;
pub type HostState = Mutex<CriticalSectionRawMutex, State<HostI2c>>;

static STATE: StaticCell<HostState> = StaticCell::new();
static TERMINATED: StaticCell<AtomicBool> = StaticCell::new();
static PSU: StaticCell<HostPsu> = StaticCell::new();

/// Open the I2C bus from `MMM_I2C_BUS`, stand up the PSU + watchdog
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
    let state: &'static _ = STATE.init(Mutex::new(State::new(i2c, MiniminerPsuConfig::DEFAULT)));
    let terminated: &'static _ = TERMINATED.init(AtomicBool::new(false));
    let psu: &'static _ = PSU.init(MiniminerPsu::new(state, terminated));
    Some((psu, state, terminated))
}

#[embassy_executor::task]
pub async fn watchdog_task(state: &'static HostState, terminated: &'static AtomicBool) {
    run_watchdog(state, terminated).await;
}

/// Cadence at which the sensor task reads INA230 input/output power.
const SENSOR_INTERVAL: Duration = Duration::from_secs(2);

/// Periodic INA230 read-out. Locks the PSU state long enough to issue
/// two I2C reads and logs the milliwatt readings.
#[embassy_executor::task]
pub async fn power_stats_task(psu: &'static HostPsu) {
    let mut ticker = Ticker::every(SENSOR_INTERVAL);
    loop {
        ticker.next().await;
        let input = psu.read_input_power_mw().await;
        let output = psu.read_output_power_mw().await;
        match (input, output) {
            (Ok(input), Ok(output)) => {
                info!("psu sensors: input={input} mW output={output} mW");
            }
            (Err(error), _) | (_, Err(error)) => {
                error!("psu sensor read failed: {error:?}");
            }
        }
    }
}
