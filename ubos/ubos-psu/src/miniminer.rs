//! `MiniminerPsu` handle, watchdog future, and `Psu` trait impl.

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_time::Ticker;
use embedded_hal_async::i2c::I2c;
use portable_atomic::{AtomicBool, Ordering};
use ubos_logging::{info, warn};

use ubos_core::units::{Power, Voltage};

use crate::state::{InitStatus, State, ensure_initialized};
use crate::{Error, Psu, ina230, tps6287x, watchdog};

#[cfg(test)]
#[path = "tests/miniminer.rs"]
mod tests;

/// `RawMutex` choice exposed for callers that need to declare the
/// `'static` mutex storage cell.
pub type RawMutex = CriticalSectionRawMutex;

/// Handle to the miniminer PSU. Cheap to clone-by-reference; the
/// underlying state lives behind a `'static` mutex supplied by the
/// caller.
pub struct MiniminerPsu<I: I2c + 'static> {
    state: &'static Mutex<RawMutex, State<I>>,
    terminated: &'static AtomicBool,
}

impl<I: I2c + 'static> MiniminerPsu<I> {
    /// Construct a handle. Spawn `run_watchdog` separately on the
    /// same `state` + `terminated` references to drive the
    /// SLG46826G heartbeat.
    pub fn new(state: &'static Mutex<RawMutex, State<I>>, terminated: &'static AtomicBool) -> Self {
        Self { state, terminated }
    }
}

/// Watchdog driver: locks the mutex once per
/// `config.watchdog_interval`, sends a heartbeat to the
/// `SLG46826G` if init has succeeded. Returns once `terminated`
/// flips to `true`.
pub async fn run_watchdog<I: I2c>(
    state: &'static Mutex<RawMutex, State<I>>,
    terminated: &'static AtomicBool,
) {
    let interval = {
        let guard = state.lock().await;
        guard.config.watchdog_interval
    };
    let mut ticker = Ticker::every(interval);
    loop {
        ticker.next().await;
        if terminated.load(Ordering::Acquire) {
            return;
        }
        let mut guard = state.lock().await;
        if guard.init == InitStatus::Initialized {
            let address = guard.config.watchdog_address;
            info!("petting the watchdog");
            if let Err(_error) = watchdog::heartbeat(&mut guard.i2c, address).await {
                warn!("psu watchdog heartbeat failed");
            }
        }
    }
}

impl<I: I2c + 'static> Psu for MiniminerPsu<I> {
    type BusError = I::Error;

    async fn enable(&self) -> Result<(), Error<I::Error>> {
        if self.terminated.load(Ordering::Acquire) {
            return Err(Error::Terminated);
        }
        let mut guard = self.state.lock().await;
        ensure_initialized(&mut guard).await?;
        let State { i2c, config, .. } = &mut *guard;
        tps6287x::enable(i2c, config).await
    }

    async fn disable(&self) -> Result<(), Error<I::Error>> {
        if self.terminated.load(Ordering::Acquire) {
            return Err(Error::Terminated);
        }
        let mut guard = self.state.lock().await;
        ensure_initialized(&mut guard).await?;
        let State { i2c, config, .. } = &mut *guard;
        tps6287x::disable(i2c, config).await
    }

    async fn set_voltage(&self, voltage: Voltage) -> Result<(), Error<I::Error>> {
        if self.terminated.load(Ordering::Acquire) {
            return Err(Error::Terminated);
        }
        let mut guard = self.state.lock().await;
        ensure_initialized(&mut guard).await?;
        let State { i2c, config, .. } = &mut *guard;
        let result = tps6287x::set_voltage(i2c, config, voltage.mv()).await;
        // Record the commanded value so telemetry consumers can compare
        // it with the INA230 bus-voltage readback. Only on success: a
        // failed write leaves the rail at the previous commanded value.
        if result.is_ok() {
            crate::record_set_voltage(voltage);
        }
        result
    }

    async fn read_input_power(&self) -> Result<Power, Error<I::Error>> {
        if self.terminated.load(Ordering::Acquire) {
            return Err(Error::Terminated);
        }
        let mut guard = self.state.lock().await;
        ensure_initialized(&mut guard).await?;
        let State { i2c, config, .. } = &mut *guard;
        ina230::read_power_mw(
            i2c,
            config.ina_input_address,
            config.ina_input_max_current_a,
        )
        .await
        .map(Power::from_mw)
        .map_err(Error::Bus)
    }

    async fn read_output_power(&self) -> Result<Power, Error<I::Error>> {
        if self.terminated.load(Ordering::Acquire) {
            return Err(Error::Terminated);
        }
        let mut guard = self.state.lock().await;
        ensure_initialized(&mut guard).await?;
        let State { i2c, config, .. } = &mut *guard;
        ina230::read_power_mw(
            i2c,
            config.ina_output_address,
            config.ina_output_max_current_a,
        )
        .await
        .map(Power::from_mw)
        .map_err(Error::Bus)
    }

    async fn read_output_voltage(&self) -> Result<Voltage, Error<I::Error>> {
        if self.terminated.load(Ordering::Acquire) {
            return Err(Error::Terminated);
        }
        let mut guard = self.state.lock().await;
        ensure_initialized(&mut guard).await?;
        let State { i2c, config, .. } = &mut *guard;
        ina230::read_bus_voltage_mv(i2c, config.ina_output_address)
            .await
            .map(Voltage::from_mv)
            .map_err(Error::Bus)
    }

    async fn read_status(&self) -> Result<crate::StatusFlags, Error<I::Error>> {
        if self.terminated.load(Ordering::Acquire) {
            return Err(Error::Terminated);
        }
        let mut guard = self.state.lock().await;
        ensure_initialized(&mut guard).await?;
        let State { i2c, config, .. } = &mut *guard;
        tps6287x::read_status(i2c, config).await
    }

    async fn terminate(&self) {
        if self.terminated.swap(true, Ordering::AcqRel) {
            return;
        }
        let mut guard = self.state.lock().await;
        if guard.init == InitStatus::Initialized {
            let State { i2c, config, .. } = &mut *guard;
            let _ = tps6287x::disable(i2c, config).await;
        }
    }
}
