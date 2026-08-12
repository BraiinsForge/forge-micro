// Copyright (C) 2026  Braiins Systems s.r.o.
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.
//
// Braiins Systems s.r.o. reserves the right to grant any party a license
// to this program, or any part thereof, under any terms, and such a grant
// shall be considered distinct from the grant above.

//! `BmmPsu` handle, watchdog future, and `Psu` trait impl.

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_time::Ticker;
use embedded_hal_async::i2c::I2c;
use portable_atomic::{AtomicBool, Ordering};
use ubos_logging::{info, warn};

use ubos_core::units::{Power, Voltage};

use crate::state::{InitStatus, State};
use crate::{BmmPsuDescriptor, Error, Psu, ina230, tps6287x, watchdog};

#[cfg(test)]
#[path = "tests/bmm.rs"]
mod tests;

/// `RawMutex` choice exposed for callers that need to declare the
/// `'static` mutex storage cell.
pub type RawMutex = CriticalSectionRawMutex;

/// Handle to the BMM PSU. Cheap to clone-by-reference; the
/// underlying state lives behind a `'static` mutex supplied by the
/// caller.
pub struct BmmPsu<I: I2c + 'static> {
    state: &'static Mutex<RawMutex, State<I, BmmPsuDescriptor>>,
    terminated: &'static AtomicBool,
}

impl<I: I2c + 'static> BmmPsu<I> {
    /// Construct a handle. Spawn `run_watchdog` separately on the
    /// same `state` + `terminated` references to drive the
    /// SLG46826G heartbeat.
    pub fn new(
        state: &'static Mutex<RawMutex, State<I, BmmPsuDescriptor>>,
        terminated: &'static AtomicBool,
    ) -> Self {
        Self { state, terminated }
    }

    /// Initialization sequence for the BMM power stage: SLG46826G
    /// watchdog, both INA230 monitors, then the `TPS6287x` converter.
    pub(crate) async fn run_init(
        i2c: &mut I,
        config: &BmmPsuDescriptor,
    ) -> Result<(), Error<I::Error>> {
        watchdog::init(i2c, config.watchdog_address)
            .await
            .map_err(Error::Bus)?;
        ina230::init(
            i2c,
            config.ina_input_address,
            config.ina_input_max_current_a,
            config.ina_input_shunt_microohm,
        )
        .await
        .map_err(Error::Bus)?;
        ina230::init(
            i2c,
            config.ina_output_address,
            config.ina_output_max_current_a,
            config.ina_output_shunt_microohm,
        )
        .await
        .map_err(Error::Bus)?;
        tps6287x::init(i2c, config).await?;
        watchdog::heartbeat(i2c, config.watchdog_address)
            .await
            .map_err(Error::Bus)
    }

    /// Replay a cached init outcome or run [`Self::run_init`] once and
    /// cache it.
    pub(crate) async fn ensure_initialized(
        state: &mut State<I, BmmPsuDescriptor>,
    ) -> Result<(), Error<I::Error>> {
        if let Some(cached) = state.cached_init() {
            return cached;
        }
        let result = Self::run_init(&mut state.i2c, &state.config).await;
        state.record_init(&result);
        result
    }
}

/// Watchdog driver: locks the mutex once per
/// `config.watchdog_interval`, sends a heartbeat to the
/// `SLG46826G` if init has succeeded. Returns once `terminated`
/// flips to `true`.
pub async fn run_watchdog<I: I2c>(
    state: &'static Mutex<RawMutex, State<I, BmmPsuDescriptor>>,
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
            info!("feeding the watchdog");
            if let Err(_error) = watchdog::heartbeat(&mut guard.i2c, address).await {
                warn!("psu watchdog heartbeat failed");
            }
        }
    }
}

impl<I: I2c + 'static> Psu for BmmPsu<I> {
    type BusError = I::Error;

    async fn enable(&self) -> Result<(), Error<I::Error>> {
        if self.terminated.load(Ordering::Acquire) {
            return Err(Error::Terminated);
        }
        let mut guard = self.state.lock().await;
        Self::ensure_initialized(&mut guard).await?;
        let State { i2c, config, .. } = &mut *guard;
        tps6287x::enable(i2c, config).await
    }

    async fn disable(&self) -> Result<(), Error<I::Error>> {
        if self.terminated.load(Ordering::Acquire) {
            return Err(Error::Terminated);
        }
        let mut guard = self.state.lock().await;
        Self::ensure_initialized(&mut guard).await?;
        let State { i2c, config, .. } = &mut *guard;
        tps6287x::disable(i2c, config).await
    }

    async fn set_voltage(&self, voltage: Voltage) -> Result<(), Error<I::Error>> {
        if self.terminated.load(Ordering::Acquire) {
            return Err(Error::Terminated);
        }
        let mut guard = self.state.lock().await;
        Self::ensure_initialized(&mut guard).await?;
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
        Self::ensure_initialized(&mut guard).await?;
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
        Self::ensure_initialized(&mut guard).await?;
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
        Self::ensure_initialized(&mut guard).await?;
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
        Self::ensure_initialized(&mut guard).await?;
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

/// The whole of a board's `psu` module where the power stage is a
/// [`BmmPsu`] on a shared I2C bus: the state alias, the `'static`
/// storage, the constructor, and the two long-running tasks.
///
/// Takes the board's bus mutex type, the `I2cDevice` handle carved from
/// it, and the machine descriptor's PSU entry. The board keeps those two
/// aliases, because they name its own HAL I2C peripheral; everything
/// built on top of them is the same on every board that has this power
/// stage.
///
/// A board whose power stage is its own wrapper rather than a [`BmmPsu`]
/// keeps its module and invokes `ubos_psu::power_stats_task!` alone.
#[macro_export]
macro_rules! i2c_psu {
    ($bus:ty, $device:ty, $descriptor:expr) => {
        /// PSU state guarded by the raw mutex `ubos_psu::BmmPsu`
        /// requires. The type is the driver's choice, not this board's.
        type PsuState = embassy_sync::mutex::Mutex<
            ubos_psu::RawMutex,
            ubos_psu::State<$device, ubos_psu::BmmPsuDescriptor>,
        >;

        /// Concrete `BmmPsu` for this board. Public so the `mining`
        /// module can spell it in `miner_task`'s signature.
        pub type BoardPsu = ubos_psu::BmmPsu<$device>;

        static STATE: static_cell::StaticCell<PsuState> = static_cell::StaticCell::new();
        static TERMINATED: static_cell::StaticCell<ubos_psu::AtomicBool> =
            static_cell::StaticCell::new();
        static PSU: static_cell::StaticCell<BoardPsu> = static_cell::StaticCell::new();

        /// Construct the `BmmPsu` over an `I2cDevice` carved from `bus`
        /// and return the `'static` references the watchdog, miner and
        /// sensor tasks need.
        #[must_use]
        pub fn init(
            bus: &'static $bus,
        ) -> (
            &'static BoardPsu,
            &'static PsuState,
            &'static ubos_psu::AtomicBool,
        ) {
            let device = embassy_embedded_hal::shared_bus::asynch::i2c::I2cDevice::new(bus);
            let state: &'static _ = STATE.init(embassy_sync::mutex::Mutex::new(
                ubos_psu::State::new(device, $descriptor),
            ));
            let terminated: &'static _ = TERMINATED.init(ubos_psu::AtomicBool::new(false));
            let psu: &'static _ = PSU.init(ubos_psu::BmmPsu::new(state, terminated));
            (psu, state, terminated)
        }

        #[embassy_executor::task]
        pub async fn watchdog_task(
            state: &'static PsuState,
            terminated: &'static ubos_psu::AtomicBool,
        ) {
            ubos_psu::run_watchdog(state, terminated).await;
        }

        ubos_psu::power_stats_task!(BoardPsu);
    };
}
