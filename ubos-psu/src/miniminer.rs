//! `MiniminerPsu` handle, watchdog future, and `Psu` trait impl.

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_time::Ticker;
use embedded_hal_async::i2c::I2c;
use portable_atomic::{AtomicBool, Ordering};

use crate::state::{ensure_initialized, InitStatus, State};
use crate::{ina230, tps6287x, watchdog, Error, Psu};

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
    pub fn new(state: &'static Mutex<RawMutex, State<I>>, terminated: &'static AtomicBool) -> Self {
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
            ubos_logging::info!("petting the watchdog");
            if let Err(_error) = watchdog::heartbeat(&mut guard.i2c, address).await {
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

    async fn set_voltage(&self, millivolts: u32) -> Result<(), Error<E>> {
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
    use crate::state::test_support;
    use embedded_hal_mock::eh1::i2c::{Mock, Transaction};

    fn run<F: core::future::Future>(future: F) -> F::Output {
        embassy_futures::block_on(future)
    }

    fn leak<T: 'static>(value: T) -> &'static T {
        Box::leak(Box::new(value))
    }

    fn make_handle(
        bus: Mock,
        config: crate::MiniminerPsuConfig,
        terminated_value: bool,
    ) -> (
        MiniminerPsu<Mock, embedded_hal_async::i2c::ErrorKind>,
        &'static Mutex<RawMutex, State<Mock>>,
    ) {
        let state: &'static _ = leak(Mutex::<RawMutex, _>::new(State::new(bus, config)));
        let terminated: &'static _ = leak(AtomicBool::new(terminated_value));
        (MiniminerPsu::new(state, terminated), state)
    }

    #[test]
    fn terminated_short_circuits_methods() {
        let bus = Mock::new(&[]);
        let config = crate::MiniminerPsuConfig::DEFAULT;
        let (psu, _state) = make_handle(bus, config, true);
        let result = run(psu.set_voltage(700));
        assert!(matches!(result, Err(Error::Terminated)));
    }

    #[test]
    fn dropped_set_voltage_future_does_not_corrupt_bus() {
        let config = crate::MiniminerPsuConfig::DEFAULT;
        let dac_700 = config
            .dcdc_voltage_range
            .voltage_to_dac(700)
            .expect("BUG: 700 mv");
        let dac_800 = config
            .dcdc_voltage_range
            .voltage_to_dac(800)
            .expect("BUG: 800 mv");

        let mut transactions = test_support::init_transactions(&config);
        transactions.push(Transaction::write(
            config.dcdc_address,
            tps6287x::encode_vset(dac_700).to_vec(),
        ));
        transactions.push(Transaction::write(
            config.dcdc_address,
            tps6287x::encode_vset(dac_800).to_vec(),
        ));

        let bus = Mock::new(&transactions);
        let (psu, state) = make_handle(bus, config, false);

        // First call runs init then writes 700 mV.
        run(psu.set_voltage(700)).expect("BUG: first call");

        // Build a second future and let it fall out of scope
        // without polling. With the Mock's synchronous I2C, a
        // single poll would run the whole transaction; instead
        // we drop pre-poll to verify that the no-poll path leaves
        // the lock + state untouched for the next caller.
        {
            let _future = psu.set_voltage(800);
        }

        // Third call must complete cleanly with the bus advancing.
        run(psu.set_voltage(800)).expect("BUG: third call");

        let mut guard = run(state.lock());
        guard.i2c.done();
    }

    #[test]
    fn terminate_disables_dcdc_and_short_circuits_subsequent_calls() {
        let config = crate::MiniminerPsuConfig::DEFAULT;
        let dac_700 = config
            .dcdc_voltage_range
            .voltage_to_dac(700)
            .expect("BUG: 700 mv");

        let mut transactions = test_support::init_transactions(&config);
        // set_voltage(700) forces init.
        transactions.push(Transaction::write(
            config.dcdc_address,
            tps6287x::encode_vset(dac_700).to_vec(),
        ));
        // terminate disables the DC/DC.
        transactions.push(Transaction::write(
            config.dcdc_address,
            tps6287x::encode_control1(false, config.dcdc_ramp_speed).to_vec(),
        ));

        let bus = Mock::new(&transactions);
        let (psu, state) = make_handle(bus, config, false);

        run(psu.set_voltage(700)).expect("BUG: set_voltage");
        run(psu.terminate());

        let post = run(psu.set_voltage(800));
        assert!(matches!(post, Err(Error::Terminated)));

        let mut guard = run(state.lock());
        guard.i2c.done();
    }
}
