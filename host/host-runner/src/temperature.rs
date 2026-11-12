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
//! Reads the four LM75A board sensors via Linux i2c-dev; the
//! controller exposes the maximum across all sensors so a single hot
//! spot wins. Mirrors the `BRAIINS_MINI_BMM101_BM1362_4X1` descriptor
//! in BOS. The sensor table is `ubos_asic::BMM101_HASHBOARD.temp`, the
//! same const the miniminer machines wrap, so it cannot drift from the
//! firmware.

use embedded_i2c_unix::I2c as HostI2c;
use log::{error, info};
use ubos_asic::BMM101_HASHBOARD;
use ubos_temperature::Sensor;
use ubos_thermal::{Accumulator, Reading, TempSample, TemperatureSensor};

use crate::psu::I2C_BUS_ENV;

pub struct HashboardTemperature {
    sensors: Vec<Sensor<HostI2c>>,
}

impl HashboardTemperature {
    /// Open a handle to the PSU I2C bus for each of the four LM75A
    /// sensors. Bus path comes from `UBOS_I2C_BUS`, defaulting to
    /// `/dev/i2c-0`. Returns `None` if the env var is set but empty
    /// or the bus cannot be opened.
    pub fn open() -> Option<Self> {
        let env_path = std::env::var(I2C_BUS_ENV);
        let path = env_path.as_deref().unwrap_or("/dev/i2c-0");
        if path.is_empty() {
            return None;
        }
        let sensors = BMM101_HASHBOARD
            .temp
            .iter()
            .map(|desc| HostI2c::open(path).map(|bus| Sensor::new(desc, bus)))
            .collect();
        let sensors = match sensors {
            Ok(sensors) => sensors,
            Err(error) => {
                error!("temperature: cannot open {path}: {error}");
                return None;
            }
        };
        info!(
            "temperature: opened {path} for {} LM75A sensor(s)",
            BMM101_HASHBOARD.temp.len()
        );
        Some(Self { sensors })
    }
}

impl TemperatureSensor for HashboardTemperature {
    async fn sample(&mut self) -> Reading {
        let mut hashboard: Option<Accumulator> = None;
        for sensor in &mut self.sensors {
            let address = sensor.descriptor().address();
            for result in sensor.read_temperatures().await {
                match result {
                    Ok(reading) => {
                        hashboard = hashboard
                            .map(|acc| acc.include(reading.value))
                            .or_else(|| Some(reading.value.into()));
                    }
                    Err(error) => error!(
                        "temperature: {:?} {address:#x} error: {:?}",
                        error.source, error.fault
                    ),
                }
            }
        }
        // Every sensor here is an LM75A on the hashboard, and the board
        // carries no die sensor, so the hashboard readings are the ones
        // this board reports.
        //
        // Never idle: these sensors sit on the board's own I2C bus and
        // answer whether or not the hashboard rail is up, so silence is
        // always a fault.
        hashboard.map_or(Reading::Unreadable, |acc| {
            Reading::Measured(TempSample {
                chip: None,
                hashboard: Some(acc.stats()),
            })
        })
    }
}
