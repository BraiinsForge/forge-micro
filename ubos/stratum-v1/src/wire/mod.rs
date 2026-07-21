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

//! Stratum V1 wire layer: framing (`line_buffer`), message types
//! (`messages`), parser (`parse`), and the JSON-RPC envelope (`rpc`).
//!
//! Usable standalone — `examples/probe.rs` connects to a real pool
//! using only types from this module. Higher-level integration that
//! plugs into `mining-protocol::MiningClient` lives in
//! `crate::client` (added in phase 5).

pub mod line_buffer;
pub mod messages;
pub mod parse;
pub mod rpc;
