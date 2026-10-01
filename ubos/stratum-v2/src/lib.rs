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

//! Stratum V2 adapter implementing `mining-protocol::MiningClient`.
//!
//! Wraps the external `micro-sv2` library, reached through the
//! `ii-sv2` umbrella crate, and exposes a protocol-agnostic mining
//! client to `stratum-client`.

#![no_std]

mod auth;
mod client;
mod config;
mod state;

pub use client::{
    ENCODE_BUFFER_BYTES, MAX_EXTENSION_PAYLOAD_BYTES, StratumV2Client, WIRE_BUFFER_BYTES,
};
pub use config::Sv2Config;
