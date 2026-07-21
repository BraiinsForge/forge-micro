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

//! No-std Stratum V1 implementation.
//!
//! Two layers live here:
//!
//! - `wire` — message types, encoders, parser, line framing. Usable
//!   standalone for protocol-only work; that's what
//!   `examples/probe.rs` exercises.
//! - The crate root re-exports the wire types for compatibility. The
//!   `MiningClient` adapter (phase 5) is added in a sibling `client`
//!   module that builds on top of `wire`.

#![no_std]
// Error types are self-documenting via Debug; panic paths don't exist in this
// no-std/no-alloc crate so these docs add no value.
#![allow(clippy::missing_errors_doc, clippy::missing_panics_doc)]
// Spec-aligned names (ParseError, EncodeError, etc.) repeat the module name
// by design; renaming would diverge from the Stratum V1 terminology.
#![allow(clippy::module_name_repetitions)]

pub mod client;
pub mod config;
pub mod error;
pub mod scratch;
pub mod state;
pub mod target;
pub mod wire;

pub use client::StratumV1Client;
pub use config::Sv1Config;

pub use error::{EncodeError, ParseError};
pub use wire::line_buffer::LineBuffer;
pub use wire::messages::{
    Authorize, ClientReconnect, Configure, ConfigureResult, Notify, SetDifficulty, SetExtranonce,
    SetVersionMask, Submit, Subscribe, SubscribeResult, encode_authorize, encode_configure,
    encode_extranonce_subscribe, encode_ping, encode_ping_response, encode_submit,
    encode_subscribe,
};
pub use wire::parse::{
    parse_boolean_result, parse_configure_result, parse_incoming, parse_subscribe_result,
    parse_version_rolling_mask,
};
pub use wire::rpc::{Method, Notification, Response, Rpc, StratumError};
