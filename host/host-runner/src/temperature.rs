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
use ubos_temperature::{Handle, SensorArray};

use crate::psu::I2C_BUS_ENV;

/// The LM75A sensors of the hashboard, each read through a handle of its
/// own to the bus.
pub type HashboardTemperature = SensorArray<{ BMM101_HASHBOARD.temp.len() }, HostI2c>;

/// Opens a handle per sensor to the bus the PSU uses. Returns `None`
/// if `UBOS_I2C_BUS` is set but empty, or the bus cannot be opened. Each
/// reading goes to `readings`.
pub fn open(readings: Handle) -> Option<HashboardTemperature> {
    let env_path = std::env::var(I2C_BUS_ENV);
    let path = env_path.as_deref().unwrap_or("/dev/i2c-0");
    if path.is_empty() {
        return None;
    }
    let buses: Result<Vec<HostI2c>, _> = BMM101_HASHBOARD
        .temp
        .iter()
        .map(|_| HostI2c::open(path))
        .collect();
    let mut buses = match buses {
        Ok(buses) => buses.into_iter(),
        Err(error) => {
            error!("temperature: cannot open {path}: {error}");
            return None;
        }
    };
    info!(
        "temperature: opened {path} for {} LM75A sensor(s)",
        BMM101_HASHBOARD.temp.len()
    );
    Some(SensorArray::new(BMM101_HASHBOARD.temp, readings, || {
        buses
            .next()
            .expect("BUG: one bus was opened for each sensor")
    }))
}
