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

//! The pool entries of version 1.

/// Selects the pool mining protocol and carries its protocol-specific
/// parameters.
#[derive(
    Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, postcard_schema::Schema,
)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum ProtocolKind {
    /// Stratum V1 with an optional worker password.
    Sv1 {
        /// Worker password, or `null` to send none; most pools authorise
        /// by worker name alone.
        #[cfg_attr(feature = "schema", schemars(with = "Option<&str>", length(max = 128)))]
        password: Option<heapless::String<128>>,
    },
    /// Stratum V2 with an optional authority public key.
    Sv2 {
        /// The pool's authority public key as an SV2 base58check string,
        /// which the client validates the pool's signature against, or
        /// `null` to validate nothing. In flash it is the raw 32 bytes.
        #[serde(with = "crate::pool::sv2_pubkey_serde")]
        #[cfg_attr(feature = "schema", schemars(with = "Option<&str>"))]
        pool_pubkey: Option<[u8; 32]>,
    },
}

/// A single pool endpoint entry.
#[derive(
    Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, postcard_schema::Schema,
)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PoolEntry {
    /// DNS name or IPv4 literal of the pool host.
    #[cfg_attr(feature = "schema", schemars(with = "str", length(max = NAME_CAPACITY)))]
    pub host: heapless::String<NAME_CAPACITY>,
    /// TCP port.
    pub port: u16,
    /// Worker name sent in `mining.authorize`.
    #[cfg_attr(feature = "schema", schemars(with = "str", length(max = NAME_CAPACITY)))]
    pub user: heapless::String<NAME_CAPACITY>,
    /// Protocol selection and its parameters.
    pub protocol: ProtocolKind,
    /// Scheduler dispatch weight for this slot. Zero marks a
    /// backup-only slot, mined only while no weighted slot has work. A
    /// missing key means zero, so a config without weights keeps the
    /// primary-plus-backups behavior.
    #[serde(default)]
    pub weight: u32,
}

/// Bytes a [`PoolEntry`] host or worker name can hold.
pub const NAME_CAPACITY: usize = 128;

/// Maximum number of pool slots the firmware persists and validates.
///
/// Drives both the `heapless::Vec<PoolEntry, MAX_POOLS>` capacity in
/// [`crate::Config`] / [`crate::ConfigUpdate`] and the length of the
/// per-slot error-message tables of [`crate::pool`], so growing the
/// slot count is a single-const change that fails to compile until
/// every table is extended in step.
pub const MAX_POOLS: usize = 3;
