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

//! Network framework for the firmware: DHCP/smoltcp glue, periodic
//! NTP sync loop, stratum TCP connect helper, and the cross-board
//! atomic state (assigned IP, NTP offset) read by the dashboard and
//! the REST handlers.
//!
//! The mDNS responder and compile-time service registry have moved to
//! `ubos-mdns`. All boards run embassy-net with smoltcp for TCP/IP.
//! The boards use `embassy-net-wiznet` in MACRAW mode so the `WIZnet`
//! chip acts as a raw MAC and smoltcp owns the stack.
#![no_std]

// The host test binary links std for the `rusty-fork` macro.
#[cfg(test)]
extern crate std;

pub mod config;
pub mod mac;
pub mod net;
pub mod ntp;

pub use net::wall_clock_secs;

/// Assert at compile time that a board's `StackResources<N>` capacity
/// covers its two HTTP pools plus the fixed net sockets
/// ([`net::min_stack_sockets`]). Every board invokes this with its chosen
/// capacity and pool sizes so the smoltcp socket-exhaustion check fires
/// uniformly rather than only on the one board that hand-rolled it.
#[macro_export]
macro_rules! assert_stack_sockets {
    ($capacity:expr, $http_pool:expr, $metrics_pool:expr $(,)?) => {
        const _: () = assert!(
            $capacity >= $crate::net::min_stack_sockets($http_pool, $metrics_pool),
            "StackResources too small: raise the capacity to cover the HTTP \
             pools plus the fixed DHCP/DNS/mDNS/NTP/stratum sockets"
        );
    };
}

ubos_events::events! {
    ip_assigned(octet0: u8, octet1: u8, octet2: u8, octet3: u8) info = "IP assigned {}.{}.{}.{}",
}
