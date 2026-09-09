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

fn store() -> &'static ChainPvt<4> {
    std::boxed::Box::leak(std::boxed::Box::new(ChainPvt::new()))
}

#[test]
fn a_chain_that_has_answered_nothing_falls_silent_on_the_clock() {
    let pvt = store();
    let sensor = ChainTemperature::new(pvt);
    // A chain still enumerating has answered nothing and is not yet
    // silent; the window runs from where the sensor started.
    assert!(!sensor.chain_is_silent(sensor.started));
    assert!(sensor.chain_is_silent(sensor.started + MAX_SILENCE));
}

#[test]
fn one_chip_answering_keeps_the_whole_chain_fresh() {
    let pvt = store();
    pvt.record(2, Some(55.5), None);
    let sensor = ChainTemperature::new(pvt);
    let now = Instant::now();
    assert!(!sensor.chain_is_silent(now));
    // Three chips have never answered, and the reading stands on the one
    // that did.
    assert_eq!(pvt.hottest_millicelsius(), Some(55_500));
}

#[test]
fn a_failed_read_does_not_hold_the_chain_open() {
    let pvt = store();
    pvt.record(0, Some(40.0), None);
    let sensor = ChainTemperature::new(pvt);
    let answered = pvt.last_answer().expect("chip 0 answered");
    // Reads that carry nothing keep arriving; the clock still runs from
    // the last real reading.
    pvt.record(0, None, None);
    assert!(sensor.chain_is_silent(answered + MAX_SILENCE));
}
