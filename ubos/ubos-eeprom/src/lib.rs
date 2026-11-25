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

//! Hashboard EEPROM support: a bus-agnostic read trait plus the
//! BOS-compatible wire format for the factory-programmed operational
//! data every hashboard carries.
//! [`forge`] parses the record that every EEPROM of a Forge machine
//! holds, whatever board it sits on.
//!
//! [`HashboardEeprom`] names no bus type, so a chip on another transport
//! implements it without [`format`] ever learning which transport
//! supplied the bytes. Vendor-specific transports and memory-map parsers
//! live in a downstream crate layered on this trait.

#![cfg_attr(not(test), no_std)]
#![allow(async_fn_in_trait)]

pub mod forge;
pub mod format;
mod hashboard_eeprom;
pub mod i2c;

pub use hashboard_eeprom::HashboardEeprom;
