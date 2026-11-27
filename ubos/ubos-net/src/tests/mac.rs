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

use super::*;
use rusty_fork::rusty_fork_test;

#[test]
fn places_oui_and_family() {
    let mac = derive_mac(DUMMY_OUI, 0x03, &[1, 2, 3, 4, 5, 6, 7, 8]);
    assert_eq!(mac[0..3], DUMMY_OUI);
    assert_eq!(mac[3], 0x03);
}

#[test]
fn dummy_oui_is_unicast_locally_administered() {
    let mac = derive_mac(DUMMY_OUI, 0x01, &[0xAB; 8]);
    // Locally-administered bit set, multicast bit clear.
    assert_eq!(mac[0] & 0x02, 0x02);
    assert_eq!(mac[0] & 0x01, 0x00);
}

#[test]
fn is_deterministic_for_same_uid() {
    let uid = [0xDE, 0xAD, 0xBE, 0xEF, 0x01, 0x02, 0x03, 0x04];
    assert_eq!(
        derive_mac(DUMMY_OUI, 0x02, &uid),
        derive_mac(DUMMY_OUI, 0x02, &uid)
    );
}

#[test]
fn distinct_uids_change_the_hash_bytes() {
    let one = derive_mac(DUMMY_OUI, 0x02, &[0x00; 8]);
    let two = derive_mac(DUMMY_OUI, 0x02, &[0xFF; 8]);
    assert_ne!(one[4..6], two[4..6]);
    // Family and OUI stay fixed regardless of uid.
    assert_eq!(one[0..4], two[0..4]);
}

#[test]
fn empty_uid_hashes_to_the_folded_offset_basis() {
    // Offset basis 0x811c_9dc5 xor-folds to 0x1cd9.
    let mac = derive_mac(DUMMY_OUI, 0x01, &[]);
    assert_eq!(mac[4], 0x1c);
    assert_eq!(mac[5], 0xd9);
}

#[test]
fn family_byte_distinguishes_boards() {
    let uid = [0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88];
    let rp2040 = derive_mac(DUMMY_OUI, 0x01, &uid);
    let rp2350 = derive_mac(DUMMY_OUI, 0x02, &uid);
    assert_ne!(rp2040, rp2350);
    assert_eq!(rp2040[4..6], rp2350[4..6]);
}

rusty_fork_test! {
    #[test]
    fn publish_mac_derives_and_publishes_on_the_handle() {
        let (publisher, net) = crate::net::Handle::take();

        let mac = publisher.publish_mac(0x07, b"chip");

        assert_eq!(mac, derive_mac(DUMMY_OUI, 0x07, b"chip"));
        assert_eq!(net.current().and_then(|state| state.mac), Some(mac));
    }
}
