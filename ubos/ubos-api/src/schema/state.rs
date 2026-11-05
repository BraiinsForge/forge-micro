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

//! Enumerations the `/api/*` bodies report as strings.
//!
//! Each enum here is the wire form of a domain enum another crate owns,
//! converted by an exhaustive `From`, so the document lists its values
//! and the domain crates carry no serde dependency.

use serde::Serialize;
use ubos_metrics::chain::ChainPresence;

/// A chain's presence as the `/api/info` `chains[].state` string.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum ApiChainState {
    /// No enumeration attempt has completed yet.
    Unknown,
    /// The chain answered with the expected chip count.
    Present,
    /// The connector answered with zero chips.
    Absent,
    /// The chain failed enumeration or its safe-boot configuration.
    Faulted,
}

impl From<ChainPresence> for ApiChainState {
    fn from(presence: ChainPresence) -> Self {
        match presence {
            ChainPresence::Unknown => Self::Unknown,
            ChainPresence::Present { .. } => Self::Present,
            ChainPresence::Absent => Self::Absent,
            ChainPresence::Faulted { .. } => Self::Faulted,
        }
    }
}
