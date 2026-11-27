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

//! Tests for `RequestCounter`.

use super::RequestCounter;
use portable_atomic::Ordering;

#[test]
fn each_recorded_request_is_counted() {
    let counter = RequestCounter::new(80);

    counter.record();
    counter.record();

    assert_eq!(counter.count.load(Ordering::Relaxed), 2);
}

#[test]
fn only_the_first_worker_claims_the_export() {
    let counter = RequestCounter::new(80);

    assert!(counter.claim_export());
    assert!(!counter.claim_export());
}
