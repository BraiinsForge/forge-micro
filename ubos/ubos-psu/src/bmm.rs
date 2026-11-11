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

use core::marker::PhantomData;

use embassy_time::Ticker;
use embedded_hal_async::i2c::I2c;
use ubos_logging::{info, warn};

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

    /// Initialization sequence for the BMM power stage: SLG46826G
    /// heartbeat, both INA230 monitors, then the `TPS6287x` converter.
    async fn init(
        i2c: &mut I,
        config: &mut BmmPsuDescriptor,
    ) -> Result<Option<ModelString>, Error<I::Error>> {
        heartbeat::init(i2c, config.heartbeat_address)
            .await
            .map_err(Error::Bus)?;
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
    /// Construct a handle. Spawn [`Self::run_watchdog`] separately on
    /// another handle over the same [`BmmShared`] to drive the SLG46826G
    /// heartbeat.
    pub fn new(shared: &'static BmmShared<I>) -> Self {
        Self { shared }
    }

    /// Heartbeat driver: takes the lock once per
    /// `config.heartbeat_interval` and sends a heartbeat to the
    /// `SLG46826G` if init has succeeded. Returns once [`Psu::terminate`]
    /// has run.
    ///
    /// A heartbeat keeps a running device alive, so it takes the lock
    /// through [`Shared::initialized`] and never brings the device up
    /// itself.
    pub async fn run_watchdog(&self) {
        let interval = self
            .shared
            .with_config(|config| config.heartbeat_interval)
            .await;
        let mut ticker = Ticker::every(interval);
        loop {
            ticker.next().await;
            if self.shared.is_terminated() {
                return;
            }
            let Some(mut psu) = self.shared.initialized().await else {
                continue;
            };
            let (i2c, config) = psu.split();
            let address = config.heartbeat_address;
            info!("sending the heartbeat");
            if let Err(_error) = heartbeat::pulse(i2c, address).await {
                warn!("psu heartbeat failed");
            }
        }
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

    async fn terminate(&self) {
        if let Some(mut psu) = self.shared.terminate().await {
            let (i2c, config) = psu.split();
            let _ = tps6287x::disable(i2c, config).await;
        }
    }
}

/// The whole of a board's `psu` module where the power stage is a
/// [`BmmPsu`] on a shared I2C bus: the `'static` storage, the
/// constructor, and the two long-running tasks.
///
/// `init` answers the driver, which only the watchdog task takes, and
/// the [`PowerSupply`](crate::PowerSupply) over it, whose handle every
/// other task takes.
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
        pub fn init(
            bus: &'static $bus,
        ) -> (&'static BoardPsu, &'static ubos_psu::PowerSupply<BoardPsu>) {
            let device = embassy_embedded_hal::shared_bus::asynch::i2c::I2cDevice::new(bus);
            let shared: &'static _ = SHARED.init(ubos_psu::BmmShared::new(device, $descriptor));
            let driver: &'static BoardPsu = DRIVER.init(ubos_psu::BmmPsu::new(shared));
            (driver, PSU.init(ubos_psu::PowerSupply::new(driver)))
        }

        #[embassy_executor::task]
        pub async fn watchdog_task(driver: &'static BoardPsu) {
            driver.run_watchdog().await;
        }

        ubos_psu::psu_task!(BoardPsu);
    };
}
