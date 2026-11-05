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

//! Schema types for the `/api/info` and `/api/system/*` REST surface.
//!
//! Each sub-module defines exactly one response struct. All structs
//! derive `serde::Serialize` and are serialized with
//! `serde-json-core` so they compile in `no_std` environments without
//! heap allocation.

pub mod generic_response;
pub mod info;
pub mod state;
pub mod system_info;
pub mod system_network;
pub mod system_scoreboard;
pub mod system_statistics;
pub mod system_temperature;

pub use generic_response::GenericResponse;
pub use info::ApiInfo;
pub use system_info::SystemInfo;
pub use system_network::SystemNetwork;
pub use system_scoreboard::SystemScoreboardEntry;
pub use system_statistics::SystemStatistics;
pub use system_temperature::SystemTemperature;

/// Schema version discriminator published on every versioned response.
///
/// Machine clients dispatch on this value to confirm they are speaking
/// to a ubos device rather than a real reference-device.
pub const API_VARIANT: &str = "ubos-0.3";
