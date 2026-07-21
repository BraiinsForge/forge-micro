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
//! lives in this crate, like [`crate::bmm`] and [`crate::bfm`], or out of tree. A
//! driver is a handle over a
//! `&'static Mutex<RawMutex, State<I, Config>>` plus a `&'static AtomicBool`
//! termination flag, and it implements [`crate::Psu`]. The recurring shape is:
//!
//! - Hold [`State`], which owns the I2C bus [`State::i2c`] and the driver's
//!   own config [`State::config`], and gates one-time device init.
//! - Run the device init sequence once, guarded by [`State::cached_init`] and
//!   recorded with [`State::record_init`]; check [`State::is_initialized`]
//!   before touching the device on teardown.
//! - Call [`crate::record_set_voltage`] after a successful `set_voltage` so the
//!   shared telemetry publisher ([`crate::run_power_stats`]) reports the
//!   commanded value.
//!
//! The concrete init sequence and the register protocol are the driver's own;
//! this module carries only the bus ownership and init bookkeeping every
//! driver would otherwise duplicate.

use embedded_hal_async::i2c::ErrorKind;

use crate::Error;

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
    // Init gate. `pub(crate)` so in-tree drivers read it directly;
    // `InitStatus` stays crate-private, so out-of-tree drivers observe
    // init state through [`Self::is_initialized`] and [`Self::cached_init`].
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
    /// bus for a device that never came up; the in-tree `bmm` and `bfm` drivers
    /// gate the same way on the crate-private `InitStatus` directly.
    #[must_use]
    pub fn is_initialized(&self) -> bool {
        self.init == InitStatus::Initialized
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use crate::BmmPsuDescriptor;
    use embedded_hal_mock::eh1::i2c::Transaction;

    /// Build the I2C transaction sequence that `run_init` produces
    /// for a given `BmmPsuDescriptor`. Shared by the integration
    /// tests so the canonical init wire shape lives next to the
    /// implementation.
    const INA_CONFIG: u16 = 0b0100_0001_0010_0111;

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
