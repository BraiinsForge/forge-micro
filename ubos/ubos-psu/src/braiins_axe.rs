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

//! `BraiinsAxePsu` handle and `Psu` trait impl for the Braiins Axe power
//! stage: one TPS53647 multiphase `PMBus` VRM. All telemetry comes
//! from the controller itself — there is no separate output monitor.

use core::marker::PhantomData;

use embedded_hal_async::i2c::I2c;
use ubos_core::units::{Power, Voltage};

use crate::state::{Device, Shared};
use crate::tps53647::{self, Tps53647Config};
use crate::{Error, Psu};

/// Runtime configuration for `BraiinsAxePsu`.
#[derive(Clone, Copy, Debug)]
pub struct BraiinsAxePsuDescriptor {
    /// TPS53647 VRM configuration.
    pub tps: Tps53647Config,
}

impl BraiinsAxePsuDescriptor {
    /// Default configuration for the Braiins Axe board.
    pub const DEFAULT: Self = Self {
        tps: Tps53647Config::DEFAULT,
    };
}

/// The Braiins Axe power stage as a [`Device`]. A marker type: it names
/// the bus and the config the guarded state is built from, and carries
/// the init sequence. Nothing constructs it.
pub struct BraiinsAxeDevice<I>(PhantomData<I>);

impl<I: I2c> Device for BraiinsAxeDevice<I> {
    type Bus = I;
    type Config = BraiinsAxePsuDescriptor;
    type BusError = I::Error;

    /// Initialization sequence for the TPS53647 VRM.
    async fn init(
        i2c: &mut I,
        config: &mut BraiinsAxePsuDescriptor,
    ) -> Result<(), Error<I::Error>> {
        tps53647::init(i2c, &config.tps).await
    }
}

/// The lock, the termination flag and the guarded state of a
/// [`BraiinsAxePsu`]. A board declares one of these and hands out
/// `&'static` references.
pub type BraiinsAxeShared<I> = Shared<BraiinsAxeDevice<I>>;

/// Handle to the Braiins Axe PSU. Cheap to clone-by-reference; the underlying
/// state lives behind a `'static` [`BraiinsAxeShared`] supplied by the
/// caller.
pub struct BraiinsAxePsu<I: I2c + 'static> {
    shared: &'static BraiinsAxeShared<I>,
}

impl<I: I2c + 'static> BraiinsAxePsu<I> {
    /// Construct a handle.
    pub fn new(shared: &'static BraiinsAxeShared<I>) -> Self {
        Self { shared }
    }
}

impl<I: I2c + 'static> Psu for BraiinsAxePsu<I> {
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
        let result = tps53647::set_voltage(i2c, &config.tps, voltage.mv()).await;
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
        tps53647::read_pin_mw(i2c, &config.tps)
            .await
            .map(Power::from_mw)
    }

    async fn read_output_power(&self) -> Result<Power, Error<I::Error>> {
        let mut psu = self.shared.acquire().await?;
        let (i2c, config) = psu.split();
        tps53647::read_pout_mw(i2c, &config.tps)
            .await
            .map(Power::from_mw)
    }

    async fn read_output_voltage(&self) -> Result<Voltage, Error<I::Error>> {
        let mut psu = self.shared.acquire().await?;
        let (i2c, config) = psu.split();
        tps53647::read_vout_mv(i2c, &config.tps)
            .await
            .map(Voltage::from_mv)
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

#[cfg(test)]
#[path = "tests/braiins_axe.rs"]
mod tests;
