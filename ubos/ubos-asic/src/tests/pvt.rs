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
    store.record(0, Some(41.5), Some(300.0));
    store.record(1, Some(38.0), Some(290.0));
    assert_eq!(store.hottest_millicelsius(), Some(41_500));
    let summary = store.voltage_summary().expect("two chips reported");
    assert_eq!(summary.chips, 2);
    assert_eq!(summary.total_microvolts, 590_000);
    assert_eq!(summary.lowest_microvolts, 290_000);
    assert_eq!(summary.highest_microvolts, 300_000);

    // A failed read leaves the last value standing, and does not count
    // as the chain answering. The whole chain would otherwise go blind
    // inside one pass over the chips whenever the wire is briefly
    // unusable, and a thermal loop reading no temperature at all halts a
    // board that is fine.
    let answered = store.last_answer();
    store.record(1, None, None);
    assert_eq!(store.hottest_millicelsius(), Some(41_500));
    assert_eq!(store.last_answer(), answered);
    assert_eq!(
        store
            .voltage_summary()
            .expect("both chips still count")
            .chips,
        2
    );

    // One quantity failing does not disturb the other.
    store.record(1, None, Some(295.0));
    let summary = store.voltage_summary().expect("two chips reported");
    assert_eq!(summary.lowest_microvolts, 295_000);
    assert_eq!(store.hottest_millicelsius(), Some(41_500));

    // A chip beyond the array is ignored rather than panicking.
    let answered = store.last_answer();
    store.record(u8::MAX, Some(90.0), Some(300.0));
    assert_eq!(
        store.last_answer(),
        answered,
        "an out-of-range chip records nothing"
    );
    assert_eq!(store.hottest_millicelsius(), Some(41_500));
}
