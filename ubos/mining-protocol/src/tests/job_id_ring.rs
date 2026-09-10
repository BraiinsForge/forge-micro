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
use crate::JobId;

#[test]
fn allocate_returns_monotonic_ids() {
    let mut ring: JobIdRing<u32, 2> = JobIdRing::new();

    let first = ring.allocate(100);
    let second = ring.allocate(101);

    assert_eq!(first, JobId(0));
    assert_eq!(second, JobId(1));
}

#[test]
fn allocated_id_maps_to_wire_id() {
    let mut ring: JobIdRing<u32, 2> = JobIdRing::new();

    let generic = ring.allocate(42);

    assert_eq!(ring.lookup(generic).copied(), Some(42));
}

#[test]
fn ring_rotates_after_depth() {
    let mut ring: JobIdRing<u32, 2> = JobIdRing::new();

    let first = ring.allocate(10);
    let second = ring.allocate(11);
    let third = ring.allocate(12);

    assert_eq!(ring.lookup(first), None);
    assert_eq!(ring.lookup(second).copied(), Some(11));
    assert_eq!(ring.lookup(third).copied(), Some(12));
}

#[test]
fn lookup_unknown_id_returns_none() {
    let mut ring: JobIdRing<u32, 2> = JobIdRing::new();

    ring.allocate(10);

    assert_eq!(ring.lookup(JobId(999)), None);
}

#[test]
fn id_counter_wraps() {
    let mut ring: JobIdRing<u32, 2> = JobIdRing::new();
    ring.next_id = u32::MAX;

    let last = ring.allocate(10);
    let wrapped = ring.allocate(11);

    assert_eq!(last, JobId(u32::MAX));
    assert_eq!(wrapped, JobId(0));
}
