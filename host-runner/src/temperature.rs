//! BMM101 hashboard temperature sensor array.
//!
//! Reads the four LM75A board sensors via Linux i2c-dev. Each reading
//! is offset by `OFFSET_C` to approximate chip junction temperature
//! from the board-mounted sensor; the controller exposes the maximum
//! across all sensors so a single hot spot wins. Mirrors the
//! `BRAIINS_MINI_BMM101_BM1362_4X1` descriptor in bos-main.

use embedded_i2c_unix::I2c as HostI2c;
use log::{error, info};
use mmm::lm75a::{Error as Lm75aError, Lm75a};
use ubos_temperature::TemperatureSensor;

use crate::psu::I2C_BUS_ENV;

/// LM75A I2C addresses on the BMM101: IC12D, IC12C, IC12B, IC12A.
const SENSOR_ADDRESSES: [u8; 4] = [0x49, 0x4a, 0x4b, 0x4c];

/// Offset added to each board reading to approximate chip junction
/// temperature. BOS uses +7 °C for every BMM101 sensor.
const OFFSET_MILLICELSIUS: i32 = 7_000;

pub struct HashboardTemperature {
    bus: HostI2c,
    sensors: [Lm75a; SENSOR_ADDRESSES.len()],
}

impl HashboardTemperature {
    /// Open a second handle to the PSU I2C bus and instantiate the four
    /// LM75A sensors. Bus path comes from `MMM_I2C_BUS`, defaulting to
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
            SENSOR_ADDRESSES.len()
        );
        Some(Self {
            bus,
            sensors: SENSOR_ADDRESSES.map(Lm75a::new),
        })
    }
}

impl TemperatureSensor for HashboardTemperature {
    async fn read_millicelsius(&mut self) -> Option<i32> {
        let mut hottest: Option<i32> = None;
        for (address, sensor) in SENSOR_ADDRESSES.iter().zip(self.sensors.iter()) {
            match sensor.read_millicelsius(&mut self.bus).await {
                Ok(reading) => {
                    let adjusted = reading + OFFSET_MILLICELSIUS;
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
        hottest
    }
}
