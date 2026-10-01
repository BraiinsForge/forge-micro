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

//! The [`Psu`] trait a board's driver implements.

use ubos_core::units::{Power, Voltage};

use crate::{Error, StatusFlags};

/// Capacity of the detected PSU model string, matching the system-info
/// API's hardware-identity string width.
pub const MODEL_LEN: usize = 40;

/// The PSU model string a driver identifies during its one-shot init.
pub type ModelString = heapless::String<MODEL_LEN>;

/// Where a measured output-rail voltage comes from. It is the `type`
/// label of the measured-voltage gauge, so every board names its
/// readings the same way and a dashboard needs no list of parts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VoltageSource {
    /// The supply's own readback of its output.
    Psu,
    /// A separate monitor on the rail.
    Feedback,
}

impl VoltageSource {
    /// The label value the gauge carries.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Psu => "psu",
            Self::Feedback => "feedback",
        }
    }
}

/// Async PSU control surface, with one implementation per hardware
/// platform.
pub trait Psu {
    type BusError: embedded_hal_async::i2c::Error;

    /// Enable the output rail, initializing the hardware if needed.
    /// An implementation may wait for startup, but success does not guarantee
    /// that the measured voltage has settled.
    async fn enable(&self) -> Result<(), Error<Self::BusError>>;
    async fn disable(&self) -> Result<(), Error<Self::BusError>>;
    /// Program the rail to `voltage`, or to the nearest value the supply
    /// can produce, and return the voltage the rail was set to.
    async fn set_voltage(&self, voltage: Voltage) -> Result<Voltage, Error<Self::BusError>>;
    async fn read_input_power(&self) -> Result<Power, Error<Self::BusError>>;
    async fn read_output_power(&self) -> Result<Power, Error<Self::BusError>>;
    /// Read the actual output rail voltage; see
    /// [`crate::PsuReading::output_voltage_measured`] for why. The source
    /// register is implementation-defined.
    async fn read_output_voltage(&self) -> Result<Voltage, Error<Self::BusError>>;
    /// Optional separate monitor on the same output rail, for
    /// cross-checking [`read_output_voltage`]. `None`, the default, means the
    /// board exposes only one source. The reading is published as
    /// [`VoltageSource::Feedback`].
    async fn read_secondary_voltage(&self) -> Option<Result<Voltage, Error<Self::BusError>>> {
        None
    }
    /// Where [`read_output_voltage`] reads the rail from.
    fn voltage_source(&self) -> VoltageSource;
    /// Read the PSU STATUS register and decode the four fault flags.
    async fn read_status(&self) -> Result<StatusFlags, Error<Self::BusError>>;
    /// The model string the driver identified, or `None` before it has
    /// and for a supply that reports none.
    async fn model(&self) -> Option<ModelString> {
        None
    }
    /// How often [`Psu::heartbeat`] is due, or `None` for a supply with
    /// no heartbeat, which is the default. [`crate::PowerSupply::run`]
    /// asks once, before its first driver call.
    async fn heartbeat_interval(&self) -> Option<embassy_time::Duration> {
        None
    }
    /// Send the supply its heartbeat. [`crate::PowerSupply::run`] calls it
    /// every [`Psu::heartbeat_interval`], so the PSU task stays the one
    /// task that reaches the driver.
    async fn heartbeat(&self) -> Result<(), Error<Self::BusError>> {
        Ok(())
    }
    async fn terminate(&self);
}
