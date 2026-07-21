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
fn status_flags_default_all_clear() {
    let flags = StatusFlags::default();
    assert!(!flags.current_limit);
    assert!(!flags.thermal_shutdown);
    assert!(!flags.overvoltage);
    assert!(!flags.undervoltage);
}

#[test]
fn error_init_failed_carries_kind() {
    let err: Error<core::convert::Infallible> =
        Error::InitFailed(embedded_hal_async::i2c::ErrorKind::Bus);
    match err {
        Error::InitFailed(kind) => {
            assert!(matches!(kind, embedded_hal_async::i2c::ErrorKind::Bus));
        }
        _ => panic!("BUG: variant mismatch"),
    }
}
