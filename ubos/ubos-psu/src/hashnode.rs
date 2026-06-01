//! `HashnodePsu` handle and `Psu` trait impl for the Braiins Hashnode miner
//! power stage: one `SIC45x` `PMBus` DC/DC converter and one INA230
//! output monitor.

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embedded_hal_async::i2c::{ErrorKind, I2c};
use portable_atomic::{AtomicBool, Ordering};
use ubos_core::units::{Power, Voltage};

use crate::state::InitStatus;
use crate::{Error, Psu, ina230, sic45x};

/// `RawMutex` choice exposed for callers that need to declare the
/// `'static` mutex storage cell.
pub type RawMutex = CriticalSectionRawMutex;

/// Runtime configuration for `HashnodePsu`.
#[derive(Clone, Copy, Debug)]
pub struct HashnodePsuConfig {
    /// `SIC45x` `PMBus` converter configuration.
    pub sic: crate::sic45x::Sic45xConfig,
    /// INA230 7-bit I2C address.
    ///
    /// The Hashnode schematic originally listed 0x43, but bench
    /// measurements and patch-0008 confirmed the populated address
    /// resistor selects 0x53. Both values are documented here so the
    /// discrepancy is traceable without digging into git history.
    pub ina_address: u8,
    /// Maximum expected load current for INA230 calibration.
    pub ina_max_current_a: u32,
    /// Shunt resistor value for INA230 calibration.
    pub ina_shunt_microohm: u32,
}

impl HashnodePsuConfig {
    /// Default configuration for the BOS Hashnode board.
    pub const DEFAULT: Self = Self {
        sic: crate::sic45x::Sic45xConfig::FEMTO,
        ina_address: 0x53,
        ina_max_current_a: 30,
        ina_shunt_microohm: 2_000,
    };
}

/// State protected by the `HashnodePsu` mutex.
pub struct HashnodeState<I> {
    pub(crate) i2c: I,
    pub(crate) config: HashnodePsuConfig,
    pub(crate) init: InitStatus,
}

impl<I> HashnodeState<I> {
    /// Construct initial state with `InitStatus::NotInit`.
    pub fn new(i2c: I, config: HashnodePsuConfig) -> Self {
        Self {
            i2c,
            config,
            init: InitStatus::NotInit,
        }
    }
}

async fn ensure_initialized<I, E>(state: &mut HashnodeState<I>) -> Result<(), Error<E>>
where
    I: I2c<Error = E>,
    E: embedded_hal_async::i2c::Error,
{
    match state.init {
        InitStatus::Initialized => return Ok(()),
        InitStatus::Failed(kind) => return Err(Error::InitFailed(kind)),
        InitStatus::NotInit => {}
    }

    if let Err(error) = run_init(state).await {
        let kind = match &error {
            Error::Bus(bus) => bus.kind(),
            // sic45x::init only returns Bus errors; this arm handles any
            // other Error variant that could reach here from future callers.
            _ => ErrorKind::Other,
        };
        state.init = InitStatus::Failed(kind);
        return Err(error);
    }

    state.init = InitStatus::Initialized;
    Ok(())
}

async fn run_init<I, E>(state: &mut HashnodeState<I>) -> Result<(), Error<E>>
where
    I: I2c<Error = E>,
    E: embedded_hal_async::i2c::Error,
{
    // Why: the INA230's VS pin is powered by the SIC output rail.
    // The rail is off during init, so the INA is below its operating
    // voltage (~2.7 V). We attempt init optimistically and continue on
    // failure: a failure here does not indicate a hardware fault on this
    // stage.
    if ina230::init(
        &mut state.i2c,
        state.config.ina_address,
        state.config.ina_max_current_a,
        state.config.ina_shunt_microohm,
    )
    .await
    .is_err()
    {
        ubos_logging::warn!("Hashnode INA230 init failed (rail not up yet?)");
    }

    sic45x::init(&mut state.i2c, &state.config.sic).await
}

/// Handle to the Hashnode PSU. Cheap to clone-by-reference; the
/// underlying state lives behind a `'static` mutex supplied by the
/// caller.
pub struct HashnodePsu<I, E>
where
    I: I2c<Error = E> + 'static,
    E: embedded_hal_async::i2c::Error + 'static,
{
    state: &'static Mutex<RawMutex, HashnodeState<I>>,
    terminated: &'static AtomicBool,
}

impl<I, E> HashnodePsu<I, E>
where
    I: I2c<Error = E> + 'static,
    E: embedded_hal_async::i2c::Error + 'static,
{
    /// Construct a handle from `'static` state and terminated flag.
    pub fn new(
        state: &'static Mutex<RawMutex, HashnodeState<I>>,
        terminated: &'static AtomicBool,
    ) -> Self {
        Self { state, terminated }
    }
}

impl<I, E> Psu for HashnodePsu<I, E>
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
        let HashnodeState { i2c, config, .. } = &mut *guard;
        sic45x::enable(i2c, &config.sic).await
    }

    async fn disable(&self) -> Result<(), Error<E>> {
        if self.terminated.load(Ordering::Acquire) {
            return Err(Error::Terminated);
        }
        let mut guard = self.state.lock().await;
        ensure_initialized(&mut guard).await?;
        let HashnodeState { i2c, config, .. } = &mut *guard;
        sic45x::disable(i2c, &config.sic).await
    }

    async fn set_voltage(&self, voltage: Voltage) -> Result<(), Error<E>> {
        if self.terminated.load(Ordering::Acquire) {
            return Err(Error::Terminated);
        }
        let mut guard = self.state.lock().await;
        ensure_initialized(&mut guard).await?;
        let HashnodeState { i2c, config, .. } = &mut *guard;
        let result = sic45x::set_voltage(i2c, &config.sic, voltage.mv()).await;
        // Record the commanded value so telemetry consumers can compare
        // it with the INA230 bus-voltage readback. Only on success: a
        // failed write leaves the rail at the previous commanded value.
        if result.is_ok() {
            crate::record_set_voltage(voltage);
        }
        result
    }

    async fn read_input_power(&self) -> Result<Power, Error<E>> {
        // The Hashnode/Femto stage has no input-side power monitor: the
        // only sensor (INA230) sits on the output rail and is reported by
        // read_output_power. Report zero so consumers leave "input" blank
        // rather than echoing the output reading under a wrong label.
        if self.terminated.load(Ordering::Acquire) {
            return Err(Error::Terminated);
        }
        Ok(Power::from_mw(0))
    }

    async fn read_output_power(&self) -> Result<Power, Error<E>> {
        // The INA230 measures the output (chip) rail directly and is the
        // accurate figure. The SIC's own READ_POUT is less accurate and
        // intentionally not reported.
        if self.terminated.load(Ordering::Acquire) {
            return Err(Error::Terminated);
        }
        let mut guard = self.state.lock().await;
        ensure_initialized(&mut guard).await?;
        let HashnodeState { i2c, config, .. } = &mut *guard;
        ina230::read_power_mw(i2c, config.ina_address, config.ina_max_current_a)
            .await
            .map(Power::from_mw)
            .map_err(Error::Bus)
    }

    async fn read_output_voltage(&self) -> Result<Voltage, Error<E>> {
        // Read from the SIC's own READ_VOUT, not the INA230: the INA is
        // powered off the output rail and is asleep until that rail
        // comes up, whereas the SIC reports VOUT as soon as VIN exists.
        if self.terminated.load(Ordering::Acquire) {
            return Err(Error::Terminated);
        }
        let mut guard = self.state.lock().await;
        ensure_initialized(&mut guard).await?;
        let HashnodeState { i2c, config, .. } = &mut *guard;
        sic45x::read_vout_mv(i2c, &config.sic)
            .await
            .map(Voltage::from_mv)
    }

    async fn read_status(&self) -> Result<crate::StatusFlags, Error<E>> {
        if self.terminated.load(Ordering::Acquire) {
            return Err(Error::Terminated);
        }
        let mut guard = self.state.lock().await;
        ensure_initialized(&mut guard).await?;
        let HashnodeState { i2c, config, .. } = &mut *guard;
        sic45x::read_status(i2c, &config.sic).await
    }

    async fn terminate(&self) {
        if self.terminated.swap(true, Ordering::AcqRel) {
            return;
        }
        let mut guard = self.state.lock().await;
        if guard.init == InitStatus::Initialized {
            let HashnodeState { i2c, config, .. } = &mut *guard;
            let _ = sic45x::disable(i2c, &config.sic).await;
        }
    }
}

#[cfg(test)]
#[path = "tests/hashnode.rs"]
mod tests;
