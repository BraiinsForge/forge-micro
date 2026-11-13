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

//! `BmmPsu` handle and `Psu` trait impl.

use core::marker::PhantomData;

use embassy_time::Duration;
use embedded_hal_async::i2c::I2c;
use ubos_logging::info;

use ubos_core::units::{Power, Voltage};

use crate::state::{Device, Shared};
use crate::{BmmPsuDescriptor, Error, ModelString, Psu, heartbeat, ina230, tps6287x};

#[cfg(test)]
#[path = "tests/bmm.rs"]
mod tests;

/// The BMM power stage as a [`Device`]. A marker type: it names the bus
/// and the config the guarded state is built from, and carries the init
/// sequence. Nothing constructs it.
pub struct BmmDevice<I>(PhantomData<I>);

impl<I: I2c> Device for BmmDevice<I> {
    type Bus = I;
    type Config = BmmPsuDescriptor;
    type BusError = I::Error;

    /// Initialization sequence for the BMM power stage: both INA230
    /// monitors, the `TPS6287x` converter, then the first `SLG46826G`
    /// heartbeat.
    async fn init(
        i2c: &mut I,
        config: &mut BmmPsuDescriptor,
    ) -> Result<Option<ModelString>, Error<I::Error>> {
        ina230::init(
            i2c,
            config.ina_input_address,
            config.ina_input_max_current,
            config.ina_input_shunt_microohm,
        )
        .await
        .map_err(Error::Bus)?;
        ina230::init(
            i2c,
            config.ina_output_address,
            config.ina_output_max_current,
            config.ina_output_shunt_microohm,
        )
        .await
        .map_err(Error::Bus)?;
        tps6287x::init(i2c, config).await?;
        heartbeat::pulse(i2c, config.heartbeat_address)
            .await
            .map_err(Error::Bus)?;
        Ok(None)
    }
}

/// The lock, the termination flag and the guarded state of a [`BmmPsu`].
/// A board declares one of these and hands out `&'static` references.
pub type BmmShared<I> = Shared<BmmDevice<I>>;

/// Handle to the BMM PSU. Cheap to clone-by-reference; the
/// underlying state lives behind a `'static` [`BmmShared`] supplied by
/// the caller.
pub struct BmmPsu<I: I2c + 'static> {
    shared: &'static BmmShared<I>,
}

impl<I: I2c + 'static> BmmPsu<I> {
    /// Construct a handle.
    pub fn new(shared: &'static BmmShared<I>) -> Self {
        Self { shared }
    }
}

impl<I: I2c + 'static> Psu for BmmPsu<I> {
    type BusError = I::Error;

    async fn enable(&self) -> Result<(), Error<I::Error>> {
        let mut psu = self.shared.acquire().await?;
        let (i2c, config) = psu.split();
        tps6287x::enable(i2c, config).await
    }

    async fn disable(&self) -> Result<(), Error<I::Error>> {
        let mut psu = self.shared.acquire().await?;
        let (i2c, config) = psu.split();
        tps6287x::disable(i2c, config).await
    }

    async fn set_voltage(&self, voltage: Voltage) -> Result<Voltage, Error<I::Error>> {
        let mut psu = self.shared.acquire().await?;
        let (i2c, config) = psu.split();
        tps6287x::set_voltage(i2c, config, voltage).await
    }

    async fn read_input_power(&self) -> Result<Power, Error<I::Error>> {
        let mut psu = self.shared.acquire().await?;
        let (i2c, config) = psu.split();
        ina230::read_power(i2c, config.ina_input_address, config.ina_input_max_current)
            .await
            .map_err(Error::Bus)
    }

    async fn read_output_power(&self) -> Result<Power, Error<I::Error>> {
        let mut psu = self.shared.acquire().await?;
        let (i2c, config) = psu.split();
        ina230::read_power(
            i2c,
            config.ina_output_address,
            config.ina_output_max_current,
        )
        .await
        .map_err(Error::Bus)
    }

    async fn read_output_voltage(&self) -> Result<Voltage, Error<I::Error>> {
        let mut psu = self.shared.acquire().await?;
        let (i2c, config) = psu.split();
        ina230::read_bus_voltage(i2c, config.ina_output_address)
            .await
            .map_err(Error::Bus)
    }

    async fn read_status(&self) -> Result<crate::StatusFlags, Error<I::Error>> {
        let mut psu = self.shared.acquire().await?;
        let (i2c, config) = psu.split();
        tps6287x::read_status(i2c, config).await
    }

    async fn heartbeat_interval(&self) -> Option<Duration> {
        Some(
            self.shared
                .with_config(|config| config.heartbeat_interval)
                .await,
        )
    }

    /// Send a heartbeat to the `SLG46826G` if init has succeeded.
    ///
    /// A heartbeat keeps a running device alive, so it takes the lock
    /// through [`Shared::initialized`] and never brings the device up
    /// itself.
    async fn heartbeat(&self) -> Result<(), Error<I::Error>> {
        if self.shared.is_terminated() {
            return Err(Error::Terminated);
        }
        let Some(mut psu) = self.shared.initialized().await else {
            return Ok(());
        };
        let (i2c, config) = psu.split();
        info!("sending the heartbeat");
        heartbeat::pulse(i2c, config.heartbeat_address)
            .await
            .map_err(Error::Bus)
    }

    async fn terminate(&self) {
        if let Some(mut psu) = self.shared.terminate().await {
            let (i2c, config) = psu.split();
            let _ = tps6287x::disable(i2c, config).await;
        }
    }
}

/// The whole of a board's `psu` module where the power stage is a
/// [`BmmPsu`] on a shared I2C bus: the `'static` storage, the
/// constructor, and the PSU task.
///
/// `init` answers the [`PowerSupply`](crate::PowerSupply) over the
/// driver, whose handle every other task takes.
///
/// Takes the board's bus mutex type, the `I2cDevice` handle carved from
/// it, and the machine descriptor's PSU entry. The board keeps those two
/// aliases, because they name its own HAL I2C peripheral; everything
/// built on top of them is the same on every board that has this power
/// stage.
///
/// A board whose power stage is its own wrapper rather than a [`BmmPsu`]
/// keeps its module and invokes `ubos_psu::psu_task!` alone.
#[macro_export]
macro_rules! bmm_psu {
    ($bus:ty, $device:ty, $descriptor:expr) => {
        /// Concrete `BmmPsu` for this board.
        pub type BoardPsu = ubos_psu::BmmPsu<$device>;

        static SHARED: static_cell::StaticCell<ubos_psu::BmmShared<$device>> =
            static_cell::StaticCell::new();
        static DRIVER: static_cell::StaticCell<BoardPsu> = static_cell::StaticCell::new();
        static PSU: static_cell::StaticCell<ubos_psu::PowerSupply<BoardPsu>> =
            static_cell::StaticCell::new();

        /// Construct the `BmmPsu` over an `I2cDevice` carved from `bus`,
        /// and the `PowerSupply` over it.
        #[must_use]
        pub fn init(bus: &'static $bus) -> &'static ubos_psu::PowerSupply<BoardPsu> {
            let device = embassy_embedded_hal::shared_bus::asynch::i2c::I2cDevice::new(bus);
            let shared: &'static _ = SHARED.init(ubos_psu::BmmShared::new(device, $descriptor));
            let driver: &'static BoardPsu = DRIVER.init(ubos_psu::BmmPsu::new(shared));
            PSU.init(ubos_psu::PowerSupply::new(driver))
        }

        ubos_psu::psu_task!(BoardPsu);
    };
}
