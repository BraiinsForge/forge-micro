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
use crate::measured;

#[test]
fn die_and_est_present() {
    // hashnode-ok: both die and PCB estimate available.
    let sample = measured(fuse(Some(90_000), Some(80_000), true));
    assert_eq!(sample.safety.millicelsius(), 90_000);
    assert_eq!(sample.safety_source, Source::Chip);
    assert_eq!(sample.pid.expect("pid present").millicelsius(), 80_000);
}

#[test]
fn die_absent_est_present_has_die_capable() {
    // hashnode remote-fault: remote diode failed, PCB estimate only.
    let sample = measured(fuse(None, Some(80_000), true));
    assert_eq!(sample.safety.millicelsius(), 80_000);
    assert_eq!(sample.safety_source, Source::Approximated);
    assert!(sample.pid.is_none());
}

#[test]
fn die_absent_est_present_no_die_capable() {
    // BMM board: no die sensor at all, PCB estimate is the only source.
    let sample = measured(fuse(None, Some(50_000), false));
    assert_eq!(sample.safety.millicelsius(), 50_000);
    assert_eq!(sample.safety_source, Source::Sole);
    assert!(sample.pid.is_none());
}

#[test]
fn die_present_est_absent() {
    // Die ok, all PCB sensors failed.
    let sample = measured(fuse(Some(95_000), None, true));
    assert_eq!(sample.safety.millicelsius(), 95_000);
    assert_eq!(sample.safety_source, Source::Chip);
    assert!(sample.pid.is_none());
}

#[test]
fn nothing_readable_with_die_capable() {
    assert!(matches!(fuse(None, None, true), Reading::Unreadable));
}

#[test]
fn nothing_readable_without_die_capable() {
    assert!(matches!(fuse(None, None, false), Reading::Unreadable));
}
