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

//! Every machine has a halt threshold for exactly the kinds of reading its
//! sensor table takes, so the thermal loop judges each reading against a
//! threshold of its own kind.

use ubos_asic::CoolingSource;
use ubos_temperature::{SensorDescriptor, TempSource};

use super::*;

/// What each channel of `sensor` measures.
fn sources(sensor: &SensorDescriptor) -> [Option<TempSource>; 5] {
    match *sensor {
        SensorDescriptor::Lm75a(part) => [Some(part.local), None, None, None, None],
        SensorDescriptor::Tmp102(part) => [Some(part.local), None, None, None, None],
        SensorDescriptor::Tmp461(part) => [Some(part.local), Some(part.remote), None, None, None],
        SensorDescriptor::Tmp464(part) => {
            let [first, second, third, fourth] = part.remote;
            [Some(part.local), first, second, third, fourth]
        }
    }
}

/// Whether `machine` has a threshold for a kind exactly when one of its
/// sensors reads that kind, and names a cooling setpoint, only for a kind
/// it has a threshold for, so its default target follows a kind it reads.
fn kinds_match_sensors<Psu: Copy>(machine: &MachineDescriptor<Psu>) -> bool {
    let reads =
        |kind: fn(TempSource) -> bool| machine.temp.iter().flat_map(sources).flatten().any(kind);
    let board = machine.hashboard;
    board.dangerous_chip_temp.is_some() == reads(|source| matches!(source, TempSource::Chip(_)))
        && board.dangerous_hashboard_temp.is_some()
            == reads(|source| source == TempSource::Hashboard)
        && match board.cooling.target_source {
            CoolingSource::Chip => board.dangerous_chip_temp.is_some(),
            CoolingSource::Hashboard => board.dangerous_hashboard_temp.is_some(),
        }
}

#[test]
fn every_machine_has_a_threshold_for_each_kind_it_reads() {
    assert!(kinds_match_sensors(&MINIMINER_RP2040));
    assert!(kinds_match_sensors(&MINIMINER_RP2350));
    assert!(kinds_match_sensors(&MINIMINER_BMM_ADAPTER));
    assert!(kinds_match_sensors(&MINIMINER_ESP32S3));
    assert!(kinds_match_sensors(&HASHNODE));
    assert!(kinds_match_sensors(&FORGE_AXE));
}
