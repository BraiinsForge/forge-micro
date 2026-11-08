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

#[test]
fn the_store() {
    let store: ChainPvt<16> = ChainPvt::new();
    assert_eq!(store.last_answer(), None, "nothing has answered yet");

    // A recorded chip contributes to both aggregates.
    store.record(
        ChipAddress::One(0),
        Some(Temperature::from_celsius_f64(41.5)),
        Some(Voltage::from_mv(300)),
    );
    store.record(
        ChipAddress::One(1),
        Some(Temperature::from_celsius_f64(38.0)),
        Some(Voltage::from_mv(290)),
    );
    assert_eq!(store.hottest().map(Temperature::millicelsius), Some(41_500));
    assert_eq!(
        store.temperature(ChipAddress::One(1)),
        Some(Temperature::from_millicelsius(38_000))
    );
    assert_eq!(
        store.temperature(ChipAddress::One(2)),
        None,
        "chip 2 has not reported"
    );
    assert_eq!(
        store.temperature(ChipAddress::One(16)),
        None,
        "chip 16 is past the array"
    );
    let dies = store.temperature_summary().expect("two chips reported");
    assert_eq!(dies.chips.get(), 2);
    assert_eq!(dies.mean.millicelsius(), 39_750);
    assert_eq!(dies.lowest.millicelsius(), 38_000);
    assert_eq!(dies.highest.millicelsius(), 41_500);
    let summary = store.voltage_summary().expect("two chips reported");
    assert_eq!(summary.chips, 2);
    assert_eq!(summary.total, Voltage::from_mv(590));
    assert_eq!(summary.lowest, Voltage::from_mv(290));
    assert_eq!(summary.highest, Voltage::from_mv(300));

    // A failed read leaves the last value standing, and does not count
    // as the chain answering. The whole chain would otherwise go blind
    // inside one pass over the chips whenever the wire is briefly
    // unusable, and a thermal loop reading no temperature at all halts a
    // board that is fine.
    let answered = store.last_answer();
    store.record(ChipAddress::One(1), None, None);
    assert_eq!(store.hottest().map(Temperature::millicelsius), Some(41_500));
    assert_eq!(store.last_answer(), answered);
    assert_eq!(
        store
            .voltage_summary()
            .expect("both chips still count")
            .chips,
        2
    );

    // One quantity failing does not disturb the other.
    store.record(ChipAddress::One(1), None, Some(Voltage::from_mv(295)));
    let summary = store.voltage_summary().expect("two chips reported");
    assert_eq!(summary.lowest, Voltage::from_mv(295));
    assert_eq!(store.hottest().map(Temperature::millicelsius), Some(41_500));

    // A chip beyond the array is ignored rather than panicking.
    let answered = store.last_answer();
    store.record(
        ChipAddress::One(u8::MAX),
        Some(Temperature::from_celsius_f64(90.0)),
        Some(Voltage::from_mv(300)),
    );
    assert_eq!(
        store.last_answer(),
        answered,
        "an out-of-range chip records nothing"
    );
    assert_eq!(store.hottest().map(Temperature::millicelsius), Some(41_500));
}

#[test]
fn all_is_the_lone_chip_of_a_chain_of_one() {
    let store: ChainPvt<1> = ChainPvt::new();
    store.record(
        ChipAddress::All,
        Some(Temperature::from_celsius_f64(50.0)),
        None,
    );
    assert_eq!(
        store.temperature(ChipAddress::One(0)),
        Some(Temperature::from_celsius_f64(50.0))
    );
}
