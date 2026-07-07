//! BMM101 hashboard temperature sensor array on the shared I2C0 bus.
//!
//! The board's LM75A sensors come from the machine descriptor's sensor
//! table and are aggregated by `ubos_temperature::SensorArray`.

use embassy_embedded_hal::shared_bus::asynch::i2c::I2cDevice;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use esp_hal::i2c::master::I2c;
use ubos_machine::MINIMINER_ESP32S3 as MACHINE;
use ubos_temperature::SensorArray;

/// The I2C0 bus is shared between the PSU and temperature sensors via a
/// bus mutex; each consumer gets an `I2cDevice` handle from the mutex.
type I2cBus = I2cDevice<'static, NoopRawMutex, I2c<'static, esp_hal::Async>>;

pub type HashboardTemperature = SensorArray<I2cBus>;

/// Construct the hashboard temperature aggregator on the shared I2C bus.
#[must_use]
pub fn new(bus: I2cBus) -> HashboardTemperature {
    SensorArray::new(bus, MACHINE.temp)
}
