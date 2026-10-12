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

//! Tests for `hw_info` — the boot-time-captured hardware-identity cells.

use super::*;

/// Publishing distinct values through all four cells round-trips each
/// back through its own getter unchanged. The cells share one payload
/// type, so a copy-paste that wired a publish function or getter to
/// the wrong cell would compile cleanly; this test is what catches it.
#[test]
fn publish_and_read_round_trip_each_cell() {
    let bootloader: HwString = "deadbeef".parse().expect("BUG: fits in HwString");
    let fan: HwString = "12.34.56 build 777".parse().expect("BUG: fits in HwString");
    let product: HwString = "Example Miner 1".parse().expect("BUG: fits in HwString");
    let mut ids: HashboardIds = Vec::new();
    ids.push(Some("SERIAL0".parse().expect("BUG: fits in HwString")))
        .expect("BUG: fits in HashboardIds");
    ids.push(None).expect("BUG: fits in HashboardIds");
    ids.push(Some("SERIAL2".parse().expect("BUG: fits in HwString")))
        .expect("BUG: fits in HashboardIds");

    publish_bootloader_version(bootloader.clone());
    publish_fan_controller_version(fan.clone());
    publish_product_name(product.clone());
    publish_hashboard_ids(ids.clone());

    assert_eq!(bootloader_version(), Some(bootloader));
    assert_eq!(fan_controller_version(), Some(fan));
    assert_eq!(product_name(), Some(product));
    assert_eq!(hashboard_ids(), Some(ids));
}
