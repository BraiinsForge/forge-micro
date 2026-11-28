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

use embassy_futures::block_on;
use embedded_hal_async::i2c::{ErrorKind, ErrorType, Operation};
use rusty_fork::rusty_fork_test;
use ubos_core::units::Temperature;

use super::*;
use crate::lm75a::Lm75a;
use crate::{Reading, Sampled, TempSource};

/// The address of the sensor that answers.
const ANSWERS: SevenBitAddress = 0x48;

/// The address of the sensor that the bus fails.
const FAILS: SevenBitAddress = 0x49;

/// The register bytes of 25 degrees.
const DEGREES_25: [u8; 2] = [25, 0];

/// A bus that answers every read of [`ANSWERS`] with [`DEGREES_25`] and
/// fails every transfer to another address.
struct Bus;

impl ErrorType for Bus {
    type Error = ErrorKind;
}

impl I2c for Bus {
    async fn transaction(
        &mut self,
        address: SevenBitAddress,
        operations: &mut [Operation<'_>],
    ) -> Result<(), ErrorKind> {
        if address != ANSWERS {
            return Err(ErrorKind::Other);
        }
        for operation in operations {
            if let Operation::Read(buffer) = operation {
                buffer.copy_from_slice(&DEGREES_25);
            }
        }
        Ok(())
    }
}

static SENSORS: [SensorDescriptor; 2] = [
    SensorDescriptor::Lm75a(Lm75a::hashboard(0, FAILS)),
    SensorDescriptor::Lm75a(Lm75a::hashboard(0, ANSWERS)),
];

// The test takes the one handle of the readings, so it runs in a process
// of its own.
rusty_fork_test! {
    #[test]
    fn a_sample_submits_each_reading_and_passes_over_a_sensor_that_fails() {
        let (mut runner, mut receiver, readings) = Handle::take();
        let mut array = SensorArray::<2, Bus>::new(&SENSORS, readings, || Bus);
        block_on(array.sample());
        block_on(runner.end_sample());
        let expected = Reading {
            source: TempSource::hashboard(ubos_core::index::HashboardIndex(0), ANSWERS),
            value: Temperature::from_celsius(25),
        };
        assert_eq!(block_on(receiver.receive()), Sampled::Reading(expected));
        assert_eq!(block_on(receiver.receive()), Sampled::End, "the failed sensor gave none");
    }
}
