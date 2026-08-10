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

//! The user-facing event ring and the decode table that reads it back.

use ubos_events::EventDef;

/// Per-crate event catalogs assembled into the registry the events
/// `/catalog` endpoint serves, so the browser can decode records into text.
///
/// Hoisted out of the board `main.rs` files, where it was duplicated
/// byte-for-byte: adding a subsystem's events meant editing every list,
/// and missing one left that board serving an incomplete decode table.
const EVENT_REGISTRY: &[&[EventDef]] = &[
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

/// Install the event ring over `backing`, and the decode table beside it.
///
/// Call this once at boot, before any task that records an event runs. The
/// backing store itself stays in the board crate, because its size follows
/// that board's capacity and its section placement follows what that MCU
/// retains across a warm reset. What is shared is the registry, the clock
/// the ring stamps records with, and the order the two installs happen in.
///
/// The clock is [`ubos_net::wall_clock_secs`], which returns uptime before
/// NTP sync and Unix time after; the ring stores whichever it gets, without
/// recording which it was.
pub fn install<const N: usize>(backing: &'static mut ubos_events::RetainedRing<N>) {
    ubos_events::set_catalogs(EVENT_REGISTRY);
    ubos_events::init(backing, ubos_net::wall_clock_secs);
}
