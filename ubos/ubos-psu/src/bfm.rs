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

//! `BfmPsu` handle and `Psu` trait impl for the Braiins Hashnode miner
//! power stage: one `SIC45x` `PMBus` DC/DC converter and one INA230
//! output monitor.

use core::fmt::Write as _;
use core::marker::PhantomData;

use embedded_hal_async::i2c::I2c;
use ubos_core::units::{Power, Voltage};

use crate::state::{Device, Shared};
use crate::{Error, Psu, ina230, sic45x};

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

/// The BFM power stage as a [`Device`]. A marker type: it names the bus
/// and the config the guarded state is built from, and carries the init
/// sequence. Nothing constructs it.
pub struct BfmDevice<I>(PhantomData<I>);

impl<I: I2c> Device for BfmDevice<I> {
    type Bus = I;
    type Config = BfmPsuDescriptor;
    type BusError = I::Error;

    /// Initialization sequence for the BFM power stage: INA230 monitor,
    /// then the `SIC45x` converter.
    async fn init(i2c: &mut I, config: &mut BfmPsuDescriptor) -> Result<(), Error<I::Error>> {
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

        let result = sic45x::init(i2c, &config.sic).await;
        if result.is_ok() {
            // Identity probe: logs the SIC450's identity registers so
            // bench captures can settle the word-vs-block framing
            // question, and yields the IC_DEVICE_ID word. The register's
            // NVM default is 0x0000 and catalog parts may ship
            // unprogrammed, so only a real value is published; a blank
            // or unreadable ID leaves the API model null. Does not
            // affect the init outcome.
            if let Some(id) = sic45x::log_identity(i2c, config.sic.address).await
                && id != 0x0000
                && id != 0xFFFF
            {
                let mut model = crate::ModelString::new();
                if write!(model, "SIC450 id={id:#06x}").is_ok() {
                    crate::publish_model(model);
                }
            }
        }
        result
    }
}

/// The lock, the termination flag and the guarded state of a [`BfmPsu`].
/// A board declares one of these and hands out `&'static` references.
pub type BfmShared<I> = Shared<BfmDevice<I>>;

/// Handle to the BFM PSU. Cheap to clone-by-reference; the
/// underlying state lives behind a `'static` [`BfmShared`] supplied by
/// the caller.
pub struct BfmPsu<I: I2c + 'static> {
    shared: &'static BfmShared<I>,
}

impl<I: I2c + 'static> BfmPsu<I> {
    /// Construct a handle.
    pub fn new(shared: &'static BfmShared<I>) -> Self {
        Self { shared }
    }
}

impl<I: I2c + 'static> Psu for BfmPsu<I> {
    type BusError = I::Error;

    async fn enable(&self) -> Result<(), Error<I::Error>> {
        let mut psu = self.shared.acquire().await?;
        let (i2c, config) = psu.split();
        sic45x::enable(i2c, &config.sic).await
    }

    async fn disable(&self) -> Result<(), Error<I::Error>> {
        let mut psu = self.shared.acquire().await?;
        let (i2c, config) = psu.split();
        sic45x::disable(i2c, &config.sic).await
    }

    async fn set_voltage(&self, voltage: Voltage) -> Result<(), Error<I::Error>> {
        let mut psu = self.shared.acquire().await?;
        let (i2c, config) = psu.split();
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
        if self.shared.is_terminated() {
            return Err(Error::Terminated);
        }
        Ok(Power::from_mw(0))
    }

    async fn read_output_power(&self) -> Result<Power, Error<I::Error>> {
        // The INA230 measures the output (chip) rail directly and is the
        // accurate figure. The SIC's own READ_POUT is less accurate and
        // intentionally not reported.
        let mut psu = self.shared.acquire().await?;
        let (i2c, config) = psu.split();
        ina230::read_power_mw(i2c, config.ina_address, config.ina_max_current_a)
            .await
            .map(Power::from_mw)
            .map_err(Error::Bus)
    }

    async fn read_output_voltage(&self) -> Result<Voltage, Error<I::Error>> {
        // Read from the SIC's own READ_VOUT, not the INA230: the INA is
        // powered off the output rail and is asleep until that rail
        // comes up, whereas the SIC reports VOUT as soon as VIN exists.
        let mut psu = self.shared.acquire().await?;
        let (i2c, config) = psu.split();
        sic45x::read_vout_mv(i2c, &config.sic)
            .await
            .map(Voltage::from_mv)
    }

    async fn read_secondary_voltage(&self) -> Option<Result<Voltage, Error<I::Error>>> {
        // Cross-check the SIC's READ_VOUT against the INA230's own bus-voltage
        // measurement of the same output rail. The INA is powered off that
        // rail, so this errors until the rail is up, and the caller drops the
        // series for that tick.
        let mut psu = match self.shared.acquire().await {
            Ok(psu) => psu,
            Err(error) => return Some(Err(error)),
        };
        let (i2c, config) = psu.split();
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
        let mut psu = self.shared.acquire().await?;
        let (i2c, config) = psu.split();
        sic45x::read_status(i2c, &config.sic).await
    }

    async fn terminate(&self) {
        if let Some(mut psu) = self.shared.terminate().await {
            let (i2c, config) = psu.split();
            let _ = sic45x::disable(i2c, &config.sic).await;
        }
    }
}

#[cfg(test)]
#[path = "tests/bfm.rs"]
mod tests;
