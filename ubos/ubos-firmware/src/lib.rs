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

#![no_std]

//! Top-of-graph firmware assembly layer.
//!
//! Holds wiring shared by every board binary that must reference the
//! functional subsystem crates. Dependency edges in the workspace flow
//! downward, so anything naming all of `ubos-config`, `ubos-thermal`,
//! `ubos-mining`, `ubos-psu`, `ubos-manager`, `ubos-api`, `ubos-net`, and
//! `stratum-client` cannot live in the foundational crates without a cycle;
//! it lives here.
//!
//! The crate is integration-level by nature: it depends on hardware-coupled
//! crates such as `ubos-mining`, so it does not host-build and carries no
//! unit tests. Anything worth testing stays in the subsystem crates below.

pub mod stratum;

pub use stratum::{MAX_POOLS, StratumShared, init_stratum, run_stratum_session};
/// Re-exported so board binaries can name `AsicSession<POOL_SLOTS>`
/// without a direct `stratum-client` dependency.
pub use stratum_client::AsicSession;

use ubos_events::EventDef;

/// Per-crate event catalogs assembled into the registry the events
/// `/catalog` endpoint serves, so the browser can decode records into text.
///
/// Hoisted out of the seven board `main.rs` files, where it was duplicated
/// byte-for-byte: adding a subsystem's events meant editing all seven lists,
/// and missing one left that board serving an incomplete decode table.
pub const EVENT_REGISTRY: &[&[EventDef]] = &[
    ubos_config::events::CATALOG,
    ubos_thermal::events::CATALOG,
    ubos_mining::events::CATALOG,
    ubos_psu::events::CATALOG,
    ubos_manager::events::CATALOG,
    ubos_api::events::CATALOG,
    ubos_net::events::CATALOG,
    stratum_client::events::CATALOG,
];

// FNV-1a slug hashes must not collide across crates, or the decode table would
// silently merge two events. Checked here, where the full registry is visible.
const _: () = assert!(
    ubos_events::codes_unique(EVENT_REGISTRY),
    "duplicate event code across catalogs"
);
