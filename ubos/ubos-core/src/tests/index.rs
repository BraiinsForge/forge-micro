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

use core::fmt::Write as _;

use heapless::String;

/// The text `args` formats to, in a fixed buffer.
fn line(args: fmt::Arguments<'_>) -> String<32> {
    let mut text = String::new();
    text.write_fmt(args).expect("the line holds the text");
    text
}

#[test]
fn range_yields_the_first_count_indices_in_order() {
    assert!(HashboardIndex::range(3).eq([0, 1, 2].map(HashboardIndex)));
}

#[test]
fn range_stops_at_the_largest_index() {
    assert_eq!(HashboardIndex::range(1000).count(), 256);
}

#[test]
fn slot_is_the_index_as_a_table_position() {
    assert_eq!(HashboardIndex(7).slot(), 7);
}

#[test]
fn display_prints_the_bare_number() {
    assert_eq!(line(format_args!("{}", HashboardIndex(2))), "2");
}

#[test]
fn display_prints_a_chip_address_as_its_chip_or_all() {
    let one = ChipAddress::One(3);
    assert_eq!(line(format_args!("{one}")), "3");
    assert_eq!(line(format_args!("{}", ChipAddress::All)), "all");
}

#[test]
fn chip_range_yields_the_first_count_chips_in_order() {
    assert!(ChipAddress::range(3).eq([0, 1, 2].map(ChipAddress::One)));
    assert_eq!(ChipAddress::range(1000).count(), 256);
}

#[test]
fn chip_slot_is_the_chip_as_a_table_position() {
    assert_eq!(ChipAddress::One(5).slot(), 5);
    assert_eq!(ChipAddress::All.slot(), 0);
}

#[test]
fn only_all_reaches_every_chip() {
    assert!(ChipAddress::All.is_all());
    assert!(!ChipAddress::One(0).is_all());
}
