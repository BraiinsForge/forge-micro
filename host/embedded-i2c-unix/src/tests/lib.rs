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

use embedded_hal_async::i2c::Error as _;

use super::*;

#[test]
fn open_nonexistent_returns_error() {
    let result = I2c::open("/dev/i2c-does-not-exist");
    assert!(result.is_err());
}

#[test]
fn error_kind_maps_io_to_other() {
    let error = Error::Io(io::Error::from(io::ErrorKind::PermissionDenied));
    assert!(matches!(
        error.kind(),
        embedded_hal_async::i2c::ErrorKind::Other
    ));
}
