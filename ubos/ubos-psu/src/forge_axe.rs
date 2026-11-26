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

//! `ForgeAxePsu` handle and `Psu` trait impl for the Forge Axe power
//! stage: one TPS53647 multiphase `PMBus` VRM. All telemetry comes
//! from the controller itself — there is no separate output monitor.

use core::marker::PhantomData;

use embassy_time::Duration;
use embedded_hal_async::i2c::{I2c, SevenBitAddress};
use ubos_core::units::{Power, Voltage};
use ubos_logging::info;
use ubos_temperature::SensorDescriptor;

use crate::state::{Device, Shared};
use crate::tps53647::{self, Tps53647Config};
use crate::{Error, ModelString, Psu, VoltageSource, heartbeat};

#[cfg(test)]
#[path = "tests/forge_axe.rs"]
mod tests;

/// Runtime configuration for `ForgeAxePsu`.
#[derive(Clone, Copy)]
pub struct ForgeAxePsuDescriptor {
    /// TPS53647 VRM configuration.
    pub tps: Tps53647Config,
    /// The temperature sensors on the PSU.
    pub temp: &'static [SensorDescriptor],
    /// I2C address of the `SLG46826G` supervisor that takes the heartbeat.
    pub heartbeat_address: SevenBitAddress,
    /// How often the supervisor is sent its heartbeat.
    pub heartbeat_interval: Duration,
}

impl ForgeAxePsuDescriptor {
    /// Default configuration for the Forge Axe board.
    pub const DEFAULT: Self = Self {
        tps: Tps53647Config::DEFAULT,
        temp: &[],
        heartbeat_address: 0x08,
        heartbeat_interval: Duration::from_secs(5),
    };
}

/// The Forge Axe power stage as a [`Device`]. A marker type: it names
/// the bus and the config the guarded state is built from, and carries
/// the init sequence. Nothing constructs it.
pub struct ForgeAxeDevice<I>(PhantomData<I>);

impl<I: I2c> Device for ForgeAxeDevice<I> {
    type Bus = I;
    type Config = ForgeAxePsuDescriptor;
    type BusError = I::Error;

    /// Initialization sequence for the TPS53647 VRM.
    async fn init(
        i2c: &mut I,
        config: &mut ForgeAxePsuDescriptor,
    ) -> Result<Option<ModelString>, Error<I::Error>> {
        tps53647::init(i2c, &config.tps).await?;
        Ok(None)
    }
}

/// The lock, the termination flag and the guarded state of a
/// [`ForgeAxePsu`]. A board declares one of these and hands out
/// `&'static` references.
pub type ForgeAxeShared<I> = Shared<ForgeAxeDevice<I>>;

/// Handle to the Forge Axe PSU. Cheap to clone-by-reference; the underlying
/// state lives behind a `'static` [`ForgeAxeShared`] supplied by the
/// caller.
pub struct ForgeAxePsu<I: I2c + 'static> {
    shared: &'static ForgeAxeShared<I>,
}

impl<I: I2c + 'static> ForgeAxePsu<I> {
    /// Construct a handle.
    pub fn new(shared: &'static ForgeAxeShared<I>) -> Self {
        Self { shared }
    }
}

impl<I: I2c + 'static> Psu for ForgeAxePsu<I> {
    type BusError = I::Error;

    async fn enable(&self) -> Result<(), Error<I::Error>> {
        let mut psu = self.shared.acquire().await?;
        let (i2c, config) = psu.split();
        tps53647::enable(i2c, &config.tps).await
    }

    async fn disable(&self) -> Result<(), Error<I::Error>> {
        let mut psu = self.shared.acquire().await?;
        let (i2c, config) = psu.split();
        tps53647::disable(i2c, &config.tps).await
    }

    async fn set_voltage(&self, voltage: Voltage) -> Result<Voltage, Error<I::Error>> {
        let mut psu = self.shared.acquire().await?;
        let (i2c, config) = psu.split();
        tps53647::set_voltage(i2c, &config.tps, voltage).await
    }

    async fn read_input_power(&self) -> Result<Power, Error<I::Error>> {
        let mut psu = self.shared.acquire().await?;
        let (i2c, config) = psu.split();
        tps53647::read_pin(i2c, &config.tps).await
    }

    async fn read_output_power(&self) -> Result<Power, Error<I::Error>> {
        let mut psu = self.shared.acquire().await?;
        let (i2c, config) = psu.split();
        tps53647::read_pout(i2c, &config.tps).await
    }

    async fn read_output_voltage(&self) -> Result<Voltage, Error<I::Error>> {
        let mut psu = self.shared.acquire().await?;
        let (i2c, config) = psu.split();
        tps53647::read_vout(i2c, &config.tps).await
    }

    fn voltage_source(&self) -> VoltageSource {
        VoltageSource::Psu
    }

    async fn read_status(&self) -> Result<crate::StatusFlags, Error<I::Error>> {
        let mut psu = self.shared.acquire().await?;
        let (i2c, config) = psu.split();
        tps53647::read_status(i2c, &config.tps).await
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
            let _ = tps53647::disable(i2c, &config.tps).await;
        }
    }
}
