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

//! Peer address screen, run before any authentication.

use core::net::{IpAddr, Ipv4Addr};

#[cfg(test)]
#[path = "tests/allow_list.rs"]
mod tests;

pub struct Cidr {
    /// Host bits must be zero.
    pub base: Ipv4Addr,
    pub prefix: u8,
}

impl Cidr {
    #[must_use]
    pub fn contains(&self, addr: Ipv4Addr) -> bool {
        if self.prefix == 0 {
            return true;
        }
        let shift = 32u32.saturating_sub(u32::from(self.prefix));
        let mask = u32::MAX << shift;
        let base = u32::from_be_bytes(self.base.octets());
        let candidate = u32::from_be_bytes(addr.octets());
        (base & mask) == (candidate & mask)
    }
}

/// RFC 1918 private ranges and link-local.
pub const DEFAULT_ALLOW_LIST: &[Cidr] = &[
    Cidr {
        base: Ipv4Addr::new(10, 0, 0, 0),
        prefix: 8,
    },
    Cidr {
        base: Ipv4Addr::new(172, 16, 0, 0),
        prefix: 12,
    },
    Cidr {
        base: Ipv4Addr::new(192, 168, 0, 0),
        prefix: 16,
    },
    Cidr {
        base: Ipv4Addr::new(169, 254, 0, 0),
        prefix: 16,
    },
];

/// An IPv4-mapped IPv6 address is checked as IPv4; a native IPv6
/// address is rejected, since no v6 LAN scope is modelled.
#[must_use]
pub fn is_allowed(ip: IpAddr, allow_list: &[Cidr]) -> bool {
    let ipv4 = match ip {
        IpAddr::V4(ipv4) => ipv4,
        IpAddr::V6(ipv6) => {
            let Some(mapped) = ipv6.to_ipv4_mapped() else {
                return false;
            };
            mapped
        }
    };
    allow_list.iter().any(|cidr| cidr.contains(ipv4))
}
