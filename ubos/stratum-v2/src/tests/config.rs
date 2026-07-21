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

//! `Sv2Config` construction tests.

use crate::config::{MAX_USER_LEN, Sv2Config, UserTooLong};

#[test]
fn new_accepts_max_length_user() {
    let user = "x".repeat(MAX_USER_LEN);
    match Sv2Config::new(&user, None) {
        Ok(config) => assert_eq!(config.user.len(), MAX_USER_LEN),
        Err(err) => panic!("max length user should fit: {err:?}"),
    }
}

#[test]
fn new_rejects_overlong_user() {
    let user = "x".repeat(MAX_USER_LEN + 1);

    assert_eq!(Sv2Config::new(&user, None).err(), Some(UserTooLong));
}
