//! `BfmPsu` handle and `Psu` trait impl for the Braiins Hashnode miner
//! power stage: one `SIC45x` `PMBus` DC/DC converter and one INA230
//! output monitor.

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embedded_hal_async::i2c::I2c;
use portable_atomic::{AtomicBool, Ordering};
use ubos_core::units::{Power, Voltage};

use crate::state::{InitStatus, PsuInit, State, ensure_initialized};
use crate::{Error, Psu, ina230, sic45x};

/// `RawMutex` choice exposed for callers that need to declare the
/// `'static` mutex storage cell.
pub type RawMutex = CriticalSectionRawMutex;

/// Runtime configuration for `BfmPsu`.
#[derive(Clone, Copy, Debug)]
pub struct BfmPsuDescriptor {
    /// `SIC45x` `PMBus` converter configuration.
    pub sic: crate::sic45x::Sic45xConfig,
    /// INA230 7-bit I2C address, strapped to 0x43 on the HASHNODE x4
    /// hardware page. An earlier config defaulted to 0x53 from a
    /// schematic reading, but the flashed firmware always overrode it to
    /// 0x43, so 0x43 is the address the hardware actually answers on.
    /// Kept as the default and documented so the 0x53/0x43 split stays
    /// traceable and is not re-raised on every review.
    pub ina_address: u8,
    /// Maximum expected load current for INA230 calibration.
    pub ina_max_current_a: u32,
    /// Shunt resistor value for INA230 calibration.
    pub ina_shunt_microohm: u32,
}

impl BfmPsuDescriptor {
    /// Default configuration for the BOS Hashnode board.
    pub const DEFAULT: Self = Self {
        sic: crate::sic45x::Sic45xConfig::DEFAULT,
        ina_address: 0x43,
        ina_max_current_a: 30,
        ina_shunt_microohm: 2_000,
    };
}

impl<I: I2c> PsuInit<I> for BfmPsuDescriptor {
    async fn run_init(i2c: &mut I, config: &Self) -> Result<(), Error<I::Error>> {
        // Why: the INA230's VS pin is powered by the SIC output rail.
        // The rail is off during init, so the INA is below its operating
        // voltage (~2.7 V). We attempt init optimistically and continue on
        // failure: a failure here does not indicate a hardware fault on this
        // stage.
        if ina230::init(
            i2c,
            config.ina_address,
            config.ina_max_current_a,
            config.ina_shunt_microohm,
        )
        .await
        .is_err()
        {
            ubos_logging::warn!("BFM INA230 init failed (rail not up yet?)");
        }

        sic45x::init(i2c, &config.sic).await
    }
}

/// State protected by the `BfmPsu` mutex. Type alias over the shared
/// [`State`] container; the generic parameter is `BfmPsuDescriptor`.
pub type BfmState<I> = State<I, BfmPsuDescriptor>;

/// Handle to the BFM PSU. Cheap to clone-by-reference; the
/// underlying state lives behind a `'static` mutex supplied by the
/// caller.
pub struct BfmPsu<I: I2c + 'static> {
    state: &'static Mutex<RawMutex, BfmState<I>>,
    terminated: &'static AtomicBool,
}

impl<I: I2c + 'static> BfmPsu<I> {
    /// Construct a handle from `'static` state and terminated flag.
    pub fn new(
        state: &'static Mutex<RawMutex, BfmState<I>>,
        terminated: &'static AtomicBool,
    ) -> Self {
        Self { state, terminated }
    }
}

impl<I: I2c + 'static> Psu for BfmPsu<I> {
    type BusError = I::Error;

    async fn enable(&self) -> Result<(), Error<I::Error>> {
        if self.terminated.load(Ordering::Acquire) {
            return Err(Error::Terminated);
        }
        let mut guard = self.state.lock().await;
        ensure_initialized(&mut guard).await?;
        let State { i2c, config, .. } = &mut *guard;
        sic45x::enable(i2c, &config.sic).await
    }

    async fn disable(&self) -> Result<(), Error<I::Error>> {
        if self.terminated.load(Ordering::Acquire) {
            return Err(Error::Terminated);
        }
        let mut guard = self.state.lock().await;
        ensure_initialized(&mut guard).await?;
        let State { i2c, config, .. } = &mut *guard;
        sic45x::disable(i2c, &config.sic).await
    }

    async fn set_voltage(&self, voltage: Voltage) -> Result<(), Error<I::Error>> {
        if self.terminated.load(Ordering::Acquire) {
            return Err(Error::Terminated);
        }
        let mut guard = self.state.lock().await;
        ensure_initialized(&mut guard).await?;
        let State { i2c, config, .. } = &mut *guard;
        let result = sic45x::set_voltage(i2c, &config.sic, voltage.mv()).await;
        // Record the commanded value so telemetry consumers can compare
        // it with the INA230 bus-voltage readback. Only on success: a
        // failed write leaves the rail at the previous commanded value.
        if result.is_ok() {
            crate::record_set_voltage(voltage);
        }
        result
    }

    async fn read_input_power(&self) -> Result<Power, Error<I::Error>> {
        // The Hashnode/Femto stage has no input-side power monitor: the
        // only sensor (INA230) sits on the output rail and is reported by
        // read_output_power. Report zero so consumers leave "input" blank
        // rather than echoing the output reading under a wrong label.
        if self.terminated.load(Ordering::Acquire) {
            return Err(Error::Terminated);
        }
        Ok(Power::from_mw(0))
    }

    async fn read_output_power(&self) -> Result<Power, Error<I::Error>> {
        // The INA230 measures the output (chip) rail directly and is the
        // accurate figure. The SIC's own READ_POUT is less accurate and
        // intentionally not reported.
        if self.terminated.load(Ordering::Acquire) {
            return Err(Error::Terminated);
        }
        let mut guard = self.state.lock().await;
        ensure_initialized(&mut guard).await?;
        let State { i2c, config, .. } = &mut *guard;
        ina230::read_power_mw(i2c, config.ina_address, config.ina_max_current_a)
            .await
            .map(Power::from_mw)
            .map_err(Error::Bus)
    }

    async fn read_output_voltage(&self) -> Result<Voltage, Error<I::Error>> {
        // Read from the SIC's own READ_VOUT, not the INA230: the INA is
        // powered off the output rail and is asleep until that rail
        // comes up, whereas the SIC reports VOUT as soon as VIN exists.
        if self.terminated.load(Ordering::Acquire) {
            return Err(Error::Terminated);
        }
        let mut guard = self.state.lock().await;
        ensure_initialized(&mut guard).await?;
        let State { i2c, config, .. } = &mut *guard;
        sic45x::read_vout_mv(i2c, &config.sic)
            .await
            .map(Voltage::from_mv)
    }

    async fn read_secondary_voltage(&self) -> Option<Result<Voltage, Error<I::Error>>> {
        // Cross-check the SIC's READ_VOUT against the INA230's own bus-voltage
        // measurement of the same output rail. The INA is powered off that
        // rail, so this errors until the rail is up, and the caller drops the
        // series for that tick.
        if self.terminated.load(Ordering::Acquire) {
            return Some(Err(Error::Terminated));
        }
        let mut guard = self.state.lock().await;
        if let Err(error) = ensure_initialized(&mut guard).await {
            return Some(Err(error));
        }
        let State { i2c, config, .. } = &mut *guard;
        Some(
            ina230::read_bus_voltage_mv(i2c, config.ina_address)
                .await
                .map(Voltage::from_mv)
                .map_err(Error::Bus),
        )
    }

    fn voltage_source(&self) -> Option<&'static str> {
        // Names the installed part, the SIC450, not the sic45x driver
        // family. This is a deployed Prometheus label value; dashboards
        // and alerts key on source="sic450", so it must not be
        // genericized to "sic45x".
        Some("sic450")
    }

    fn secondary_voltage_source(&self) -> Option<&'static str> {
        Some("ina230")
    }

    async fn read_status(&self) -> Result<crate::StatusFlags, Error<I::Error>> {
        if self.terminated.load(Ordering::Acquire) {
            return Err(Error::Terminated);
        }
        let mut guard = self.state.lock().await;
        ensure_initialized(&mut guard).await?;
        let State { i2c, config, .. } = &mut *guard;
        sic45x::read_status(i2c, &config.sic).await
    }

    async fn terminate(&self) {
        if self.terminated.swap(true, Ordering::AcqRel) {
            return;
        }
        let mut guard = self.state.lock().await;
        if guard.init == InitStatus::Initialized {
            let State { i2c, config, .. } = &mut *guard;
            let _ = sic45x::disable(i2c, &config.sic).await;
        }
    }
}

#[cfg(test)]
#[path = "tests/bfm.rs"]
mod tests;
