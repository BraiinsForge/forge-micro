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

//! mDNS responder loop plus the `ServiceEntry` type that crates use to
//! advertise themselves.
//!
//! Each crate that owns a network-visible port declares an entry at
//! module scope via [`declare_service!`]. The macro expands to a plain
//! `const ServiceEntry`. Firmware composes a `&[&ServiceEntry]` slice
//! at the [`run_mdns`] call site, so the firmware decides which
//! services the board actually advertises — no linker contract, no
//! startup copy.
#![no_std]

pub mod registry;
pub mod responder;

pub use registry::ServiceEntry;
pub use responder::{compose_hostname, compose_hostname_configured, run_mdns};
