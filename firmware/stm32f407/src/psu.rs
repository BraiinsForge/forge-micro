//! BMM100 PSU integration over the shared I2C1 bus.
//!
//! Owns the static cells for the `MiniminerPsu` state, the terminated flag,
//! and the PSU handle. The bus mutex is owned by `main` so future I2C
//! consumers can take additional `I2cDevice` handles from the same mutex
//! without going through this module.

use defmt::{error, info, Debug2Format};
use embassy_embedded_hal::shared_bus::asynch::i2c::I2cDevice;
use embassy_embedded_hal::shared_bus::I2cDeviceError;
use embassy_stm32::i2c::{Error as I2cError, I2c, Master};
use embassy_sync::blocking_mutex::raw::{CriticalSectionRawMutex, NoopRawMutex};
use embassy_sync::mutex::Mutex;
use embassy_time::{Duration, Ticker};
use portable_atomic::AtomicBool;
use static_cell::StaticCell;
use ubos_psu::{run_watchdog, MiniminerPsu, MiniminerPsuConfig, Psu, State};

/// Bus mutex shared by every consumer on I2C1. `NoopRawMutex` is sufficient
/// because the STM32F407 is single-core: every I2C consumer runs on the same
/// executor and there is no cross-core contention.
pub type Bus = Mutex<NoopRawMutex, I2c<'static, embassy_stm32::mode::Async, Master>>;

/// `embedded-hal-async` device handle for one consumer of the shared bus.
type Device = I2cDevice<'static, NoopRawMutex, I2c<'static, embassy_stm32::mode::Async, Master>>;

/// PSU state guarded by the raw mutex `ubos_psu::MiniminerPsu` requires.
type PsuState = Mutex<CriticalSectionRawMutex, State<Device>>;
/// Concrete `MiniminerPsu` instantiation for the BMM Adapter. Public so the
/// `mining` module can spell it in `miner_task`'s signature.
pub type BoardPsu = MiniminerPsu<Device, I2cDeviceError<I2cError>>;

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

/// Cadence at which the sensor task reads INA230 input/output power. Matches
/// the host-runner cadence so the prometheus series rates are comparable.
const SENSOR_INTERVAL: Duration = Duration::from_secs(2);

#[embassy_executor::task]
pub async fn power_stats_task(psu: &'static BoardPsu) -> ! {
    let mut ticker = Ticker::every(SENSOR_INTERVAL);
    loop {
        ticker.next().await;
        let input = psu.read_input_power_mw().await;
        let output = psu.read_output_power_mw().await;
        match (input, output) {
            (Ok(input), Ok(output)) => {
                info!("psu sensors: input={} mW output={} mW", input, output);
            }
            (Err(error), Ok(_)) | (Ok(_), Err(error)) => {
                error!("psu sensor read failed: {:?}", Debug2Format(&error));
            }
            (Err(input_error), Err(output_error)) => {
                error!(
                    "psu sensor read failed: input={:?} output={:?}",
                    Debug2Format(&input_error),
                    Debug2Format(&output_error)
                );
            }
        }
    }
}
