//! Host-runner PSU exercise: drives `MiniminerPsu` against a real
//! BMM100 hashboard wired to a Linux i2c-dev bus.
//!
//! Activated by setting `HOST_PSU_I2C=/dev/i2c-N`. Skipped when the
//! env var is unset; opening the bus failure also keeps the rest of
//! the runner alive.

use embassy_executor::{SpawnError, SpawnToken, Spawner};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_time::{Duration, Timer};
use embedded_i2c_unix::I2c as HostI2c;
use portable_atomic::AtomicBool;
use static_cell::StaticCell;
use ubos_psu::{MiniminerPsu, MiniminerPsuConfig, Psu, State, run_watchdog};

type StateMutex = Mutex<CriticalSectionRawMutex, State<HostI2c>>;

static STATE: StaticCell<StateMutex> = StaticCell::new();
static TERMINATED: StaticCell<AtomicBool> = StaticCell::new();

#[embassy_executor::task]
async fn watchdog_task(
    state: &'static StateMutex,
    terminated: &'static AtomicBool,
) {
    run_watchdog(state, terminated).await;
}

#[embassy_executor::task]
async fn exercise_task(
    psu: MiniminerPsu<HostI2c, embedded_i2c_unix::Error>,
) {
    log::info!("psu: enabling");
    if let Err(error) = psu.enable().await {
        log::error!("psu enable: {error:?}");
        return;
    }

    log::info!("psu: set 600 mV");
    if let Err(error) = psu.set_voltage(600).await {
        log::error!("psu set_voltage(600): {error:?}");
        psu.terminate().await;
        return;
    }
    Timer::after(Duration::from_secs(1)).await;
    log_power(&psu).await;

    log::info!("psu: set 700 mV");
    if let Err(error) = psu.set_voltage(700).await {
        log::error!("psu set_voltage(700): {error:?}");
        psu.terminate().await;
        return;
    }
    Timer::after(Duration::from_secs(1)).await;
    log_power(&psu).await;

    log::info!("psu: disabling");
    let _ = psu.disable().await;
    psu.terminate().await;
    log::info!("psu exercise complete");
}

async fn log_power<P: Psu>(psu: &P) {
    match psu.read_input_power_mw().await {
        Ok(milliwatts) => log::info!("psu input: {milliwatts} mW"),
        Err(_) => log::warn!("psu input power read failed"),
    }
    match psu.read_output_power_mw().await {
        Ok(milliwatts) => log::info!("psu output: {milliwatts} mW"),
        Err(_) => log::warn!("psu output power read failed"),
    }
}

/// Spawn the exercise + watchdog tasks if `HOST_PSU_I2C` is set.
///
/// Returns the (watchdog, exercise) spawn tokens so main.rs follows
/// the same `spawner.spawn(task(...).expect(...))` shape it uses for
/// every other task.
pub fn spawn_if_enabled(
    spawner: Spawner,
) -> Option<(
    Result<SpawnToken<impl Sized>, SpawnError>,
    Result<SpawnToken<impl Sized>, SpawnError>,
)> {
    let path = std::env::var("HOST_PSU_I2C").ok()?;
    if path.is_empty() {
        return None;
    }

    let i2c = match HostI2c::open(&path) {
        Ok(i2c) => i2c,
        Err(error) => {
            log::error!("psu exercise: cannot open {path}: {error}");
            return None;
        }
    };

    let config = MiniminerPsuConfig::DEFAULT;
    let state: &'static _ =
        STATE.init(Mutex::new(State::new(i2c, config)));
    let terminated: &'static _ =
        TERMINATED.init(AtomicBool::new(false));
    let psu = MiniminerPsu::new(state, terminated);

    let _ = spawner;
    Some((
        watchdog_task(state, terminated),
        exercise_task(psu),
    ))
}
