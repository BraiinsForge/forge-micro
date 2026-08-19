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

//! BMM101 hashboard temperature sensor array.
//!
//! Reads the four LM75A board sensors via Linux i2c-dev. Each reading
//! is offset by the descriptor's `pcb_to_die_offset_mc` to approximate
//! chip junction temperature from the board-mounted sensor; the
//! controller exposes the maximum across all sensors so a single hot
//! spot wins. Mirrors the `BRAIINS_MINI_BMM101_BM1362_4X1` descriptor
//! in BOS. The sensor table is `ubos_machine::BMM101_SENSORS`, the same
//! const the miniminer machines wrap, so it cannot drift from the
//! firmware.

use embedded_i2c_unix::I2c as HostI2c;
use log::{error, info};
use ubos_core::units::Temperature;
use ubos_machine::BMM101_SENSORS;
use ubos_temperature::lm75a::{Error as Lm75aError, Lm75a};
use ubos_thermal::{Sample, TemperatureSensor};

use crate::psu::I2C_BUS_ENV;

pub struct HashboardTemperature {
    bus: HostI2c,
    sensors: [Lm75a; BMM101_SENSORS.len()],
}

impl HashboardTemperature {
    /// Open a second handle to the PSU I2C bus and instantiate the four
    /// LM75A sensors. Bus path comes from `UBOS_I2C_BUS`, defaulting to
    /// `/dev/i2c-0`. Returns `None` if the env var is set but empty
    /// or the bus cannot be opened.
    pub fn open() -> Option<Self> {
        let env_path = std::env::var(I2C_BUS_ENV);
        let path = env_path.as_deref().unwrap_or("/dev/i2c-0");
        if path.is_empty() {
            return None;
        }
        let bus = match HostI2c::open(path) {
            Ok(bus) => bus,
            Err(error) => {
                error!("temperature: cannot open {path}: {error}");
                return None;
            }
        };
        info!(
            "temperature: opened {path} for {} LM75A sensor(s)",
            BMM101_SENSORS.len()
        );
        Some(Self {
            bus,
            sensors: BMM101_SENSORS.map(|desc| Lm75a::new(desc.address)),
        })
    }
}

impl TemperatureSensor for HashboardTemperature {
    async fn sample(&mut self) -> Option<Sample> {
        let mut hottest: Option<i32> = None;
        for (desc, sensor) in BMM101_SENSORS.iter().zip(self.sensors.iter()) {
            let address = desc.address;
            match sensor.read_millicelsius(&mut self.bus).await {
                Ok(reading) => {
                    let adjusted = reading + desc.pcb_to_die_offset_mc;
                    hottest = Some(hottest.map_or(adjusted, |current| current.max(adjusted)));
                }
                Err(Lm75aError::I2c(error)) => {
                    error!("temperature: LM75A {address:#x} I2C error: {error:?}");
                }
                Err(Lm75aError::OutOfRange(raw)) => {
                    error!("temperature: LM75A {address:#x} reading {raw} mC out of range");
                }
            }
        }
        // Every sensor here is a PCB LM75A with an offset, and the board
        // carries no die sensor to contrast them against, so the hottest
        // adjusted reading is the one estimate this board reports.
        hottest
            .map(Temperature::from_millicelsius)
            .map(Sample::sole)
    }
}
