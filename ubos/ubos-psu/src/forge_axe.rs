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

use embedded_hal_async::i2c::I2c;
use ubos_core::units::{Power, Voltage};

use crate::state::{Device, Shared};
use crate::tps53647::{self, Tps53647Config};
use crate::{Error, Psu};

#[cfg(test)]
#[path = "tests/forge_axe.rs"]
mod tests;

/// Runtime configuration for `ForgeAxePsu`.
#[derive(Clone, Copy, Debug)]
pub struct ForgeAxePsuDescriptor {
    /// TPS53647 VRM configuration.
    pub tps: Tps53647Config,
}

impl ForgeAxePsuDescriptor {
    /// Default configuration for the Forge Axe board.
    pub const DEFAULT: Self = Self {
        tps: Tps53647Config::DEFAULT,
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
    async fn init(i2c: &mut I, config: &mut ForgeAxePsuDescriptor) -> Result<(), Error<I::Error>> {
        tps53647::init(i2c, &config.tps).await
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

    async fn set_voltage(&self, voltage: Voltage) -> Result<(), Error<I::Error>> {
        let mut psu = self.shared.acquire().await?;
        let (i2c, config) = psu.split();
        let result = tps53647::set_voltage(i2c, &config.tps, voltage).await;
        // Record the commanded value so telemetry consumers can compare
        // it with the READ_VOUT readback. Only on success: a failed
        // write leaves the rail at the previous commanded value.
        if result.is_ok() {
            crate::record_set_voltage(voltage);
        }
        result
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

    fn voltage_source(&self) -> Option<&'static str> {
        Some("tps53647")
    }

    async fn read_status(&self) -> Result<crate::StatusFlags, Error<I::Error>> {
        let mut psu = self.shared.acquire().await?;
        let (i2c, config) = psu.split();
        tps53647::read_status(i2c, &config.tps).await
    }

    async fn terminate(&self) {
        if let Some(mut psu) = self.shared.terminate().await {
            let (i2c, config) = psu.split();
            let _ = tps53647::disable(i2c, &config.tps).await;
        }
    }
}
