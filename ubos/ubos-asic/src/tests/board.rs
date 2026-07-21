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

use ubos_core::units::Voltage;

#[test]
fn bmm101_max_rail_voltage_clamps_to_psu_ceiling() {
    // Chip stack 4 * 422 mV = 1688 mV exceeds the BMM101_HASHBOARD PSU's
    // configured upper bound 1675 mV; max_rail_voltage clamps to the
    // PSU ceiling so Psu::set_voltage never returns Error::OutOfRange
    // when the tuner anchors the open-core rail at this value.
    assert_eq!(BMM101_HASHBOARD.max_rail_voltage(), Voltage::from_mv(1675));
}
