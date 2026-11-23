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

//! Unit tests for the source identity.

use super::*;

#[test]
fn a_hashboard_sensor_names_its_own_address_in_its_source() {
    let chain = HashboardIndex(1);
    let source = TempSource::Hashboard {
        chain,
        address: 0x48,
    };
    assert_eq!(Lm75a::hashboard(chain.0, 0x48).local(), source);
    assert_eq!(Tmp102::hashboard(chain.0, 0x48).local(), source);
    assert_eq!(
        SensorDescriptor::Lm75a(Lm75a::hashboard(chain.0, 0x48)).address(),
        0x48
    );
}

#[test]
fn a_remote_diode_measures_a_chip_of_the_chain_of_its_sensor() {
    let chain = HashboardIndex(1);
    let chip = ChipAddress::One(5);
    let sensor = Tmp461::hashboard(chain.0, 0x4E, chip);
    assert_eq!(sensor.remote(), TempSource::Chip { chain, chip });
    let sensor = Tmp464::hashboard(chain.0, 0x4A, [Some(chip), None, None, None]);
    assert_eq!(
        sensor.remote(),
        [Some(TempSource::Chip { chain, chip }), None, None, None]
    );
}

#[test]
fn a_supply_sensor_measures_the_power_supply() {
    assert_eq!(Tmp102::psu(0x49).local(), TempSource::Psu);
}
