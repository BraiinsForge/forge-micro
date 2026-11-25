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

//! Hashnode PSU integration over the shared I2C1 bus.
//!
//! The `SIC45x` here is gated by the `SIC_EN` pin, not by the `PMBus`
//! `OPERATION` register this device rejects, so [`GatedPsu`] is built with
//! no settle delay, no power-good input and no inner `enable`/`disable`;
//! register reads and `set_voltage` still delegate to the inner driver.

use embassy_embedded_hal::shared_bus::asynch::i2c::I2cDevice;
use embassy_rp::gpio::{Input, Output};
use static_cell::StaticCell;
use ubos_machine::HASHNODE as MACHINE;
use ubos_psu::{BfmPsu, BfmShared, GatedPsu, InnerGate, PowerSupply};
use ubos_rp::i2c::{Bus, Device};

type InnerPsu = BfmPsu<Device>;

/// Board power stage: the `SIC45x` driver gated by the `SIC_EN` pin
/// alone.
pub type BoardPsu = GatedPsu<InnerPsu, Output<'static>, Input<'static>, 1>;

static SHARED: StaticCell<BfmShared<Device>> = StaticCell::new();
static DRIVER: StaticCell<BoardPsu> = StaticCell::new();
static PSU: StaticCell<PowerSupply<BoardPsu>> = StaticCell::new();

/// Takes the `SIC_EN` pin already driven low, and returns the
/// `PowerSupply` whose handle every other task takes.
pub fn init(bus: &'static Bus, sic_enable: Output<'static>) -> &'static PowerSupply<BoardPsu> {
    let device = I2cDevice::new(bus);
    let shared: &'static _ = SHARED.init(BfmShared::new(device, MACHINE.psu));
    let inner = BfmPsu::new(shared);
    let driver: &'static BoardPsu = DRIVER.init(GatedPsu::new(
        inner,
        [sic_enable],
        None,
        None,
        InnerGate::PinsOnly,
    ));
    PSU.init(PowerSupply::new(driver))
}

ubos_psu::psu_task!(BoardPsu);
