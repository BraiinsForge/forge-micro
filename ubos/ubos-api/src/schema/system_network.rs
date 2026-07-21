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

//! Response schema for `GET /api/system/network`.

use heapless::Vec;
use serde::Serialize;

/// Live network status returned by `GET /api/system/network`.
///
/// `mode` reflects the currently configured IP assignment mode.
/// `pending` is `true` while a live change is awaiting operator
/// confirmation. `seconds_remaining` counts down to the automatic
/// revert; it is 0 when no change is pending.
///
/// The address fields carry the live values from the embassy-net stack,
/// which works for both modes: static returns the configured values,
/// DHCP returns the current lease. All fields are `None` / empty when
/// the stack has not yet acquired an address.
#[derive(Serialize)]
pub struct SystemNetwork<'a> {
    /// Current network mode: `"dhcp"` or `"static"`.
    pub mode: &'a str,
    /// Whether a live change is awaiting confirmation.
    pub pending: bool,
    /// Seconds remaining in the confirm window, or 0 when nothing is pending.
    pub seconds_remaining: u64,
    /// IPv4 address as four octets, or `null` when no address is assigned.
    pub address: Option<[u8; 4]>,
    /// Network prefix length (1–32), or `null` when no address is assigned.
    pub prefix_len: Option<u8>,
    /// Default gateway as four octets, or `null` when none is configured.
    pub gateway: Option<[u8; 4]>,
    /// DNS resolver addresses; each entry is four octets. Empty when none are configured.
    pub dns: Vec<[u8; 4], 3>,
}
