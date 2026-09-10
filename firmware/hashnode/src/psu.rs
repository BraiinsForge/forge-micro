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
//! Wraps the shared `SIC45x` `PMBus` driver in [`GatedPsu`], which owns
//! the `SIC_EN` (GPIO30) hardware-enable pin. On this board the `SIC45x`
//! is gated by that pin, not by the `PMBus` `OPERATION` register, which
//! this device rejects, so `GatedPsu` is built with no settle delay, no
//! power-good input, and no inner `enable`/`disable` call; register
//! reads and `set_voltage` still delegate to the inner driver. The bus
//! mutex is owned by `main` so other I2C consumers can take additional
//! `I2cDevice` handles from the same mutex.

use embassy_embedded_hal::shared_bus::asynch::i2c::I2cDevice;
use embassy_rp::gpio::{Input, Output};
use embassy_rp::i2c::{Async, I2c};
use embassy_rp::peripherals::I2C1;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::mutex::Mutex;
use static_cell::StaticCell;
use ubos_machine::HASHNODE as MACHINE;
use ubos_psu::{BfmPsu, BfmShared, GatedPsu, InnerGate};

/// Bus mutex shared by every consumer on I2C1. `NoopRawMutex` is sufficient
/// because every I2C consumer runs on the Core 0 executor. The chip-serial
/// actor (`mining::serial_task`) runs on Core 1 but does not touch I2C, so
/// there is no cross-core contention. If a future I2C consumer moves to
/// Core 1, bump this to `CriticalSectionRawMutex`.
pub type Bus = Mutex<NoopRawMutex, I2c<'static, I2C1, Async>>;

/// `embedded-hal-async` device handle for one consumer of the shared bus.
/// Public because the hashboard sensor array takes one of these too.
pub type Device = I2cDevice<'static, NoopRawMutex, I2c<'static, I2C1, Async>>;

/// The shared `SIC45x` `PMBus` driver instantiated for this board's bus.
type InnerPsu = BfmPsu<Device>;

/// Board power stage: the `SIC45x` driver gated by the `SIC_EN` pin
/// alone. Public so the `mining` module can spell it in `miner_task`'s
/// signature.
pub type BoardPsu = GatedPsu<InnerPsu, Output<'static>, Input<'static>, 1>;

static SHARED: StaticCell<BfmShared<Device>> = StaticCell::new();
static PSU: StaticCell<BoardPsu> = StaticCell::new();

/// Construct the `BoardPsu` over an `I2cDevice` carved from `bus`, taking
/// ownership of the `SIC_EN` pin (passed in already driven low), and return
/// the `'static` reference the miner and power-stats tasks need.
pub fn init(bus: &'static Bus, sic_enable: Output<'static>) -> &'static BoardPsu {
    let device = I2cDevice::new(bus);
    let shared: &'static _ = SHARED.init(BfmShared::new(device, MACHINE.psu));
    let inner = BfmPsu::new(shared);
    PSU.init(GatedPsu::new(
        inner,
        [sic_enable],
        None,
        None,
        InnerGate::PinsOnly,
    ))
}

ubos_psu::power_stats_task!(BoardPsu);
