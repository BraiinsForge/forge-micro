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

//! BMM101 PSU integration over the shared I2C0 bus.
//!
//! Owns the static cells for the shared `BmmPsu` state and the PSU
//! handle, both of which `ubos_psu::bmm_psu!` writes. The bus mutex is
//! owned by `main`, so another consumer takes its own `I2cDevice` from
//! the same mutex rather than one of these; the shared bus types
//! themselves are `ubos_esp32::i2c::Bus` and `Device`, re-exported here
//! so `main` names one module for everything on the bus.

pub use ubos_esp32::i2c::{Bus, Device};

ubos_psu::bmm_psu!(Bus, Device, crate::config::MACHINE.psu);
