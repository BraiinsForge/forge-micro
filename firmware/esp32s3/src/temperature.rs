//! BMM101 hashboard temperature sensor array.
//!
//! Reads four LM75A board sensors over I2C0. Mirrors `stm32f407/src/temperature.rs`
//! with the I2C bus type swapped to `esp_hal::i2c::master::I2c<'static, esp_hal::Async>`.

use defmt::error;
use embassy_embedded_hal::shared_bus::asynch::i2c::I2cDevice;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use esp_hal::i2c::master::I2c;
use ubos_temperature::lm75a::{Error as Lm75aError, Lm75a};
use ubos_temperature::TemperatureSensor;

/// LM75A I2C addresses on the BMM101: IC12D, IC12C, IC12B, IC12A.
const SENSOR_ADDRESSES: [u8; 4] = [0x49, 0x4a, 0x4b, 0x4c];

/// Offset added to each board reading to approximate chip junction
/// temperature. BOS uses +7 °C for every BMM101 sensor.
const OFFSET_MILLICELSIUS: i32 = 7_000;

/// The I2C0 bus is shared between the PSU and temperature sensors via a
/// bus mutex; each consumer gets an `I2cDevice` handle from the mutex.
type I2cBus = I2cDevice<'static, NoopRawMutex, I2c<'static, esp_hal::Async>>;

/// Reads the hottest of the four on-board LM75A sensors and applies the
/// junction-temperature offset.
pub struct HashboardTemperature {
    bus: I2cBus,
    sensors: [Lm75a; SENSOR_ADDRESSES.len()],
}

impl HashboardTemperature {
    #[must_use]
    pub fn new(bus: I2cBus) -> Self {
        Self {
            bus,
            sensors: SENSOR_ADDRESSES.map(Lm75a::new),
        }
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
                Err(Lm75aError::I2c(bus_error)) => {
                    error!(
                        "temperature: LM75A {:#x} I2C error: {:?}",
                        address, bus_error
                    );
                }
                Err(Lm75aError::OutOfRange(raw)) => {
                    error!(
                        "temperature: LM75A {:#x} reading {} mC out of range",
                        address, raw
                    );
                }
            }
        }
        hottest
    }
}
