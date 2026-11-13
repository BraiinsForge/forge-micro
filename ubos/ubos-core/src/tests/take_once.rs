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
fn first_take_succeeds() {
    TakeOnce::new("test").take();
}

#[test]
#[should_panic(expected = "the test handle was already taken")]
fn second_take_panics() {
    let once = TakeOnce::new("test");
    once.take();
    once.take();
}

#[test]
fn flags_are_independent() {
    TakeOnce::new("test").take();
    TakeOnce::new("test").take();
}
