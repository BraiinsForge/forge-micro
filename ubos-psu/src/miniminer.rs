//! `MiniminerPsu` handle, watchdog future, and `Psu` trait impl.

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_time::Ticker;
use embedded_hal_async::i2c::I2c;
use portable_atomic::{AtomicBool, Ordering};

use crate::state::{InitStatus, State, ensure_initialized};
use crate::{Error, Psu, ina230, tps6287x, watchdog};

/// `RawMutex` choice exposed for callers that need to declare the
/// `'static` mutex storage cell.
pub type RawMutex = CriticalSectionRawMutex;

/// Handle to the miniminer PSU. Cheap to clone-by-reference; the
/// underlying state lives behind a `'static` mutex supplied by the
/// caller.
pub struct MiniminerPsu<I, E>
where
    I: I2c<Error = E> + 'static,
    E: embedded_hal_async::i2c::Error + 'static,
{
    state: &'static Mutex<RawMutex, State<I>>,
    terminated: &'static AtomicBool,
}

impl<I, E> MiniminerPsu<I, E>
where
    I: I2c<Error = E> + 'static,
    E: embedded_hal_async::i2c::Error + 'static,
{
    /// Construct a handle. Spawn `run_watchdog` separately on the
    /// same `state` + `terminated` references to drive the
    /// SLG46826G heartbeat.
    pub fn new(
        state: &'static Mutex<RawMutex, State<I>>,
        terminated: &'static AtomicBool,
    ) -> Self {
        Self { state, terminated }
    }
}

/// Watchdog driver: locks the mutex once per
/// `config.watchdog_interval`, sends a heartbeat to the
/// `SLG46826G` if init has succeeded. Returns once `terminated`
/// flips to `true`.
pub async fn run_watchdog<I, E>(
    state: &'static Mutex<RawMutex, State<I>>,
    terminated: &'static AtomicBool,
) where
    I: I2c<Error = E>,
    E: embedded_hal_async::i2c::Error,
{
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
            if let Err(_error) =
                watchdog::heartbeat(&mut guard.i2c, address).await
            {
                #[cfg(feature = "defmt")]
                defmt::warn!("psu watchdog heartbeat failed");
            }
        }
    }
}

impl<I, E> Psu for MiniminerPsu<I, E>
where
    I: I2c<Error = E> + 'static,
    E: embedded_hal_async::i2c::Error + 'static,
{
    type BusError = E;

    async fn enable(&self) -> Result<(), Error<E>> {
        if self.terminated.load(Ordering::Acquire) {
            return Err(Error::Terminated);
        }
        let mut guard = self.state.lock().await;
        ensure_initialized(&mut guard).await?;
        let State { i2c, config, .. } = &mut *guard;
        tps6287x::enable(i2c, config).await
    }

    async fn disable(&self) -> Result<(), Error<E>> {
        if self.terminated.load(Ordering::Acquire) {
            return Err(Error::Terminated);
        }
        let mut guard = self.state.lock().await;
        ensure_initialized(&mut guard).await?;
        let State { i2c, config, .. } = &mut *guard;
        tps6287x::disable(i2c, config).await
    }

    async fn set_voltage(
        &self,
        millivolts: u32,
    ) -> Result<(), Error<E>> {
        if self.terminated.load(Ordering::Acquire) {
            return Err(Error::Terminated);
        }
        let mut guard = self.state.lock().await;
        ensure_initialized(&mut guard).await?;
        let State { i2c, config, .. } = &mut *guard;
        tps6287x::set_voltage(i2c, config, millivolts).await
    }

    async fn read_input_power_mw(&self) -> Result<u32, Error<E>> {
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
        .map_err(Error::Bus)
    }

    async fn read_output_power_mw(&self) -> Result<u32, Error<E>> {
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
        .map_err(Error::Bus)
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

#[cfg(test)]
mod tests {
    use super::*;
    use embedded_hal_mock::eh1::i2c::Mock;

    fn run<F: core::future::Future>(future: F) -> F::Output {
        embassy_futures::block_on(future)
    }

    fn leak<T: 'static>(value: T) -> &'static T {
        Box::leak(Box::new(value))
    }

    #[test]
    fn terminated_short_circuits_methods() {
        let bus = Mock::new(&[]);
        let config = crate::MiniminerPsuConfig::DEFAULT;
        let state: &'static _ = leak(Mutex::<RawMutex, _>::new(
            State::new(bus, config),
        ));
        let terminated: &'static _ = leak(AtomicBool::new(true));
        let psu = MiniminerPsu::new(state, terminated);
        let result = run(psu.set_voltage(700));
        assert!(matches!(result, Err(Error::Terminated)));
    }
}
