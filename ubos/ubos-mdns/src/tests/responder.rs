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

use static_cell::StaticCell;

use super::{HOSTNAME_SUFFIX_LEN, compose_hostname_configured};

const BASE: &str = "ab";
const BASE_LEN: usize = 2;
const CAP: usize = BASE_LEN + HOSTNAME_SUFFIX_LEN;

#[test]
fn empty_configured_returns_composed() {
    static CELL: StaticCell<heapless::String<CAP>> = StaticCell::new();
    let hostname = compose_hostname_configured::<CAP, BASE_LEN>("", BASE, 0xef, &CELL);
    assert!(
        hostname.contains(BASE),
        "expected base in hostname, got {hostname}"
    );
    assert!(
        hostname.contains("ef"),
        "expected hex suffix in hostname, got {hostname}"
    );
}

#[test]
fn short_configured_returned_verbatim() {
    static CELL: StaticCell<heapless::String<CAP>> = StaticCell::new();
    let hostname = compose_hostname_configured::<CAP, BASE_LEN>("hello", BASE, 0x01, &CELL);
    assert_eq!(hostname, "hello");
}

#[test]
fn over_long_configured_falls_back_to_composed() {
    static CELL: StaticCell<heapless::String<CAP>> = StaticCell::new();
    let hostname = compose_hostname_configured::<CAP, BASE_LEN>("toolong", BASE, 0xab, &CELL);
    assert!(
        hostname.contains(BASE),
        "expected fallback to composed hostname, got {hostname}",
    );
    assert!(
        hostname.contains("ab"),
        "expected hex suffix in fallback hostname, got {hostname}"
    );
}
