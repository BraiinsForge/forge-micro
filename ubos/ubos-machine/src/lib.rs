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

//! Machine descriptors: the miner modeled as composed data.
//!
//! A [`MachineDescriptor`] ties the component descriptors owned by the
//! subsystem crates to the [`ControlBoardDescriptor`] and
//! [`PlatformDescriptor`] for the board the firmware runs on.
//!
//! The crate is pure `const` data with no hardware or async
//! dependencies, so the firmware binaries and the host runner build from
//! the same definitions. The firmware binary supplies the typed
//! peripheral bindings this data cannot express; see `docs/MACHINE.md`.

#![no_std]

pub mod control_board;
pub mod forge_axe;
pub mod front_panel;
pub mod machine;
pub mod platform;

pub use control_board::*;
pub use front_panel::*;
pub use machine::*;
pub use platform::*;
