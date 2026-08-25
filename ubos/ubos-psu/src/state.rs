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

//! State container and initialization gating shared by every PSU driver.
//!
//! This module is the support interface a PSU driver builds on, whether it
//! lives in this crate, like [`crate::bmm`] and [`crate::bfm`], or out of tree.
//! A driver is a handle over a `&'static Shared<D>` and it implements
//! [`crate::Psu`]. The recurring shape is:
//!
//! - Hold [`Shared`], which owns the lock, the termination flag, the I2C bus
//!   and the driver's own config, and gates one-time device init.
//! - Describe the device with [`Device`], whose [`Device::init`] is the one
//!   part that differs between drivers.
//! - Open every method with [`Shared::acquire`], which refuses a terminated
//!   driver, takes the lock, runs init once, and hands back a [`Ready`].
//!   Reach the bus and the config through [`Ready::split`] and never through
//!   the guard.
//! - Call [`crate::record_set_voltage`] after a successful `set_voltage` so the
//!   shared telemetry publisher ([`crate::run_power_stats`]) reports the
//!   commanded value.
//!
//! Two callers want a different protocol and get their own method.
//! [`Shared::terminate`] must proceed where `acquire` refuses, and
//! [`Shared::initialized`] must not bring the device up.
//!
//! The concrete init sequence and the register protocol are the driver's own;
//! this module carries the lock protocol, the bus ownership and the init
//! bookkeeping every driver would otherwise duplicate.

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::{Mutex, MutexGuard};
use embedded_hal_async::i2c::ErrorKind;
use portable_atomic::{AtomicBool, Ordering};

use crate::Error;

/// The raw mutex every PSU driver in this crate locks with.
///
/// One choice for the whole crate, so no board picks from its own sharing
/// scope. A `critical_section` costs tens of cycles and the I2C transaction
/// under it costs hundreds of microseconds, so the saving from a per-board
/// choice is below a tenth of a percent, and the price is a per-board proof
/// that one executor reaches the PSU.
pub type RawMutex = CriticalSectionRawMutex;

#[cfg(test)]
#[path = "tests/state.rs"]
mod tests;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InitStatus {
    NotInit,
    Initialized,
    Failed(ErrorKind),
}

/// State protected by a PSU mutex. Generic over the I2C bus `I` and the
/// config data `C`. The init routine that consumes them is a driver
/// method; [`Self::cached_init`] and [`Self::record_init`] carry the
/// shared bookkeeping around it.
pub struct State<I, C> {
    /// The I2C bus the driver talks to the device over. A driver locks the
    /// enclosing mutex and reaches the bus through this field.
    pub i2c: I,
    /// Driver-defined configuration or discovered device data. `BmmPsu`
    /// stores its `BmmPsuDescriptor`; another driver might store a
    /// calibration constant it discovers during device identification.
    pub config: C,
    // Init gate. `pub(crate)` so [`Shared`]'s own methods reach it, and
    // `InitStatus` stays crate-private, so every driver observes init
    // state through [`Self::is_initialized`] and [`Self::cached_init`].
    pub(crate) init: InitStatus,
}

impl<I, C> State<I, C> {
    pub fn new(i2c: I, config: C) -> Self {
        Self {
            i2c,
            config,
            init: InitStatus::NotInit,
        }
    }

    /// Cached outcome of a previous init attempt, or `None` when no
    /// attempt has run yet and the driver must run its init sequence.
    pub fn cached_init<E>(&self) -> Option<Result<(), Error<E>>> {
        match self.init {
            InitStatus::Initialized => Some(Ok(())),
            InitStatus::Failed(kind) => Some(Err(Error::InitFailed(kind))),
            InitStatus::NotInit => None,
        }
    }

    /// Record the outcome of an init attempt for [`Self::cached_init`].
    pub fn record_init<E>(&mut self, result: &Result<(), Error<E>>)
    where
        E: embedded_hal_async::i2c::Error,
    {
        self.init = match result {
            Ok(()) => InitStatus::Initialized,
            Err(Error::Bus(bus)) => InitStatus::Failed(bus.kind()),
            // Init only constructs Bus or OutOfRange (the latter
            // when initial_mv is misconfigured); collapse the
            // misconfig case into Other.
            Err(_) => InitStatus::Failed(ErrorKind::Other),
        };
    }

    /// Whether init has run and succeeded. An out-of-tree driver's `terminate`
    /// checks this before a final device-off command, so it does not touch the
    /// bus for a device that never came up. The in-tree drivers get the same
    /// check from [`Shared::terminate`] and [`Shared::initialized`], which call
    /// this on their behalf.
    #[must_use]
    pub fn is_initialized(&self) -> bool {
        self.init == InitStatus::Initialized
    }
}

/// The one part of the lock protocol that differs between drivers: the
/// types the guarded state is built from, and the device init sequence.
///
/// A driver implements this on a marker type and holds a [`Shared`] over
/// it. Everything else about locking, termination and init caching is
/// [`Shared`]'s.
pub trait Device {
    /// The I2C bus the driver reaches the device over.
    type Bus;
    /// Driver-defined configuration and discovered device data.
    type Config;
    /// The bus error this driver reports.
    type BusError: embedded_hal_async::i2c::Error;

    /// Bring the device up. Runs at most once per [`Shared`], under the
    /// lock, and the outcome is cached for every later caller.
    ///
    /// The config is mutable because device identification can discover a
    /// value the driver stores back, such as a scaling constant.
    async fn init(
        bus: &mut Self::Bus,
        config: &mut Self::Config,
    ) -> Result<(), Error<Self::BusError>>;
}

/// The lock, the termination flag and the guarded state of one PSU
/// driver. A driver holds a `&'static Shared<D>` and nothing else.
///
/// Both the lock and the flag are private, so the four-step prologue every
/// driver method needs cannot be spelled by hand or partly forgotten.
/// [`Shared::acquire`] is that prologue.
pub struct Shared<D: Device> {
    state: Mutex<RawMutex, State<D::Bus, D::Config>>,
    /// Set by [`Shared::terminate`] before it takes the lock, and read
    /// twice by [`Shared::acquire`] and once under the lock by
    /// [`Shared::initialized`]. One storage location is the truth.
    terminated: AtomicBool,
}

/// A held lock over a device that init has brought up. Returned by
/// [`Shared::acquire`], and by [`Shared::terminate`] and
/// [`Shared::initialized`] when the device is up.
///
/// The guard is private: a caller reaches the device through
/// [`Ready::split`] and cannot reach the init gate or the raw state.
pub struct Ready<'a, D: Device> {
    guard: MutexGuard<'a, RawMutex, State<D::Bus, D::Config>>,
}

impl<D: Device> Shared<D> {
    /// Build the guarded state. The device is not touched here; init runs
    /// on the first [`Shared::acquire`].
    pub fn new(bus: D::Bus, config: D::Config) -> Self {
        Self {
            state: Mutex::new(State::new(bus, config)),
            terminated: AtomicBool::new(false),
        }
    }

    /// Whether [`Shared::terminate`] has run. A hint outside the lock, and
    /// the authoritative answer only where the lock is held.
    #[must_use]
    pub fn is_terminated(&self) -> bool {
        self.terminated.load(Ordering::Acquire)
    }

    /// Refuse a terminated driver, take the lock, refuse it again, and run
    /// the device init once.
    ///
    /// The flag is read twice on purpose, and both reads are of the same
    /// storage. The first rejects a terminated caller before it queues on
    /// the lock. The second is the authoritative one: [`Shared::terminate`]
    /// sets the flag before it takes the lock, so a caller that queued
    /// behind a termination sees it here instead of driving a device that
    /// was deliberately stopped.
    ///
    /// # Errors
    ///
    /// [`Error::Terminated`] when the driver is stopped, on either read of
    /// the flag. Whatever [`Device::init`] returned on the attempt that ran
    /// the device init, or [`Error::InitFailed`] for every caller after an
    /// attempt that failed.
    pub async fn acquire(&self) -> Result<Ready<'_, D>, Error<D::BusError>> {
        if self.is_terminated() {
            return Err(Error::Terminated);
        }
        let mut guard = self.state.lock().await;
        if self.is_terminated() {
            return Err(Error::Terminated);
        }
        if let Some(cached) = guard.cached_init() {
            cached?;
        } else {
            let result = {
                let State { i2c, config, .. } = &mut *guard;
                D::init(i2c, config).await
            };
            guard.record_init(&result);
            result?;
        }
        Ok(Ready { guard })
    }

    /// Take the lock without running init, and hand back the device only
    /// when a previous init succeeded.
    ///
    /// This is the watchdog's entry point. A heartbeat keeps a running
    /// device alive and must never be what brings one up.
    ///
    /// Returns `None` for a terminated driver, read under the lock for the
    /// same reason [`Shared::acquire`] reads it there. The init status is a
    /// cache that [`Shared::terminate`] does not clear, so it cannot answer
    /// this on its own: a heartbeat that queued before the termination would
    /// otherwise reach a device that was deliberately stopped.
    pub async fn initialized(&self) -> Option<Ready<'_, D>> {
        let guard = self.state.lock().await;
        if self.is_terminated() {
            return None;
        }
        if guard.is_initialized() {
            Some(Ready { guard })
        } else {
            None
        }
    }

    /// Read from the driver config under the lock, without running init.
    ///
    /// The watchdog needs its own interval before the device is up, which
    /// is the one read that neither [`Shared::acquire`] nor
    /// [`Shared::initialized`] can serve.
    pub async fn with_config<T>(&self, read: impl FnOnce(&D::Config) -> T) -> T {
        read(&self.state.lock().await.config)
    }

    /// Stop the driver: set the flag, then take the lock.
    ///
    /// Returns the device only when init had succeeded and there is
    /// therefore something on the bus to switch off, and `None` when the
    /// device never came up or a previous call already terminated. This is
    /// the one caller that must proceed where [`Shared::acquire`] refuses.
    pub async fn terminate(&self) -> Option<Ready<'_, D>> {
        if self.terminated.swap(true, Ordering::AcqRel) {
            return None;
        }
        let guard = self.state.lock().await;
        if guard.is_initialized() {
            Some(Ready { guard })
        } else {
            None
        }
    }
}

impl<D: Device> Ready<'_, D> {
    /// The bus and the config together.
    ///
    /// One call rather than two accessors, because a driver method needs
    /// both at once: the bus borrows the guard mutably and the config
    /// borrows it shared, and both borrows are live across the call.
    pub fn split(&mut self) -> (&mut D::Bus, &D::Config) {
        let State { i2c, config, .. } = &mut *self.guard;
        (i2c, config)
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use crate::BmmPsuDescriptor;
    use embedded_hal_mock::eh1::i2c::Transaction;

    const INA_CONFIG: u16 = 0b0100_0001_0010_0111;

    /// Build the I2C transaction sequence that `Device::init` produces
    /// for a given `BmmPsuDescriptor`. Shared by the integration
    /// tests so the canonical init wire shape lives next to the
    /// implementation.
    pub fn init_transactions(config: &BmmPsuDescriptor) -> Vec<Transaction> {
        use crate::{ina230, tps6287x};

        let dac = config
            .dcdc_voltage_range
            .voltage_to_dac(config.dcdc_initial.mv())
            .expect("BUG: initial mv must be in vrange");
        let cal_in = ina230::calibration_value(
            ina230::current_lsb_microamps(config.ina_input_max_current_a),
            config.ina_input_shunt_microohm,
        );
        let cal_out = ina230::calibration_value(
            ina230::current_lsb_microamps(config.ina_output_max_current_a),
            config.ina_output_shunt_microohm,
        );

        let mut buffer_in_config = vec![0x00u8];
        buffer_in_config.extend_from_slice(&INA_CONFIG.to_be_bytes());
        let mut buffer_in_cal = vec![0x05u8];
        buffer_in_cal.extend_from_slice(&cal_in.to_be_bytes());
        let mut buffer_out_config = vec![0x00u8];
        buffer_out_config.extend_from_slice(&INA_CONFIG.to_be_bytes());
        let mut buffer_out_cal = vec![0x05u8];
        buffer_out_cal.extend_from_slice(&cal_out.to_be_bytes());

        vec![
            Transaction::write(config.watchdog_address, vec![0x7a, 0x00]),
            Transaction::write(config.ina_input_address, buffer_in_config),
            Transaction::write(config.ina_input_address, buffer_in_cal),
            Transaction::write(config.ina_output_address, buffer_out_config),
            Transaction::write(config.ina_output_address, buffer_out_cal),
            Transaction::write(
                config.dcdc_address,
                tps6287x::encode_control1_reset().to_vec(),
            ),
            Transaction::write(
                config.dcdc_address,
                tps6287x::encode_control1(false, config.dcdc_ramp_speed).to_vec(),
            ),
            Transaction::write(
                config.dcdc_address,
                tps6287x::encode_control2(config.dcdc_voltage_range, config.dcdc_soft_start)
                    .to_vec(),
            ),
            Transaction::write(config.dcdc_address, tps6287x::encode_vset(dac).to_vec()),
            Transaction::write(config.watchdog_address, vec![0x7a, 0x01]),
            Transaction::write(config.watchdog_address, vec![0x7a, 0x00]),
        ]
    }
}
