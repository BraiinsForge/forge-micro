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

//! Forge Axe PSU integration over the shared I2C1 bus.
//!
//! The TPS53647 needs both the ENABLE pin and the `OPERATION` on-bit, so
//! [`GatedPsu`] owns `PWR_EN` and the `VR_RDY` power-good input and drives
//! the inner driver's `OPERATION` write.

use embassy_embedded_hal::shared_bus::asynch::i2c::I2cDevice;
use embassy_rp::gpio::{Input, Output};
use static_cell::StaticCell;
use ubos_psu::{
    ForgeAxePsu, ForgeAxePsuDescriptor, ForgeAxeShared, GatedPsu, InnerGate, PowerSupply,
};
use ubos_rp::i2c::{Bus, Device};

type InnerPsu = ForgeAxePsu<Device>;

/// Board power stage: the TPS53647 driver gated by `PWR_EN` with a
/// `VR_RDY` power-good check.
pub type BoardPsu = GatedPsu<InnerPsu, Output<'static>, Input<'static>, 1>;

static SHARED: StaticCell<ForgeAxeShared<Device>> = StaticCell::new();
static DRIVER: StaticCell<BoardPsu> = StaticCell::new();
static PSU: StaticCell<PowerSupply<BoardPsu>> = StaticCell::new();

/// Covers the soft-start ramp between raising the enable pins and sampling
/// `VR_RDY`.
const VR_RDY_SETTLE: embassy_time::Duration = embassy_time::Duration::from_millis(10);

/// Construct the `BoardPsu` over an `I2cDevice` carved from `bus`,
/// configured by `descriptor`,
/// taking ownership of `PWR_EN` (passed in already driven low), and
/// return the `PowerSupply` over it, whose handle every other task takes.
pub fn init(
    bus: &'static Bus,
    descriptor: ForgeAxePsuDescriptor,
    pwr_en: Output<'static>,
    vr_rdy: Input<'static>,
) -> &'static PowerSupply<BoardPsu> {
    let device = I2cDevice::new(bus);
    let shared: &'static _ = SHARED.init(ForgeAxeShared::new(device, descriptor));
    let inner = ForgeAxePsu::new(shared);
    let driver: &'static BoardPsu = DRIVER.init(GatedPsu::new(
        inner,
        [pwr_en],
        Some(vr_rdy),
        Some(VR_RDY_SETTLE),
        InnerGate::DriveOperation,
    ));
    PSU.init(PowerSupply::new(driver))
}

ubos_psu::psu_task!(BoardPsu);
