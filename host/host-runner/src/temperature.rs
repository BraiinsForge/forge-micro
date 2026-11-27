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

//! BMM101 hashboard temperature sensor array over Linux i2c-dev.
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
use ubos_thermal::{Handle as ThermalHandle, TemperatureSensor};

use crate::psu::I2C_BUS_ENV;

pub struct HashboardTemperature {
    sensors: Vec<Sensor<HostI2c>>,
    /// Where each reading is submitted, so thermal filters it and writes
    /// its series.
    thermal: ThermalHandle,
}

impl HashboardTemperature {
    /// Opens a handle per sensor to the bus the PSU uses. Returns `None`
    /// if `UBOS_I2C_BUS` is set but empty, or the bus cannot be opened.
    /// Each reading goes to `thermal`.
    pub fn open(thermal: ThermalHandle) -> Option<Self> {
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
        Some(Self { sensors, thermal })
    }
}

impl TemperatureSensor for HashboardTemperature {
    async fn sample(&mut self) {
        for sensor in &mut self.sensors {
            let address = sensor.descriptor().address();
            for result in sensor.read_temperatures().await {
                match result {
                    Ok(reading) => self.thermal.submit(reading).await,
                    Err(error) => error!(
                        "temperature: {:?} {address:#x} error: {:?}",
                        error.source, error.fault
                    ),
                }
            }
        }
    }
}
