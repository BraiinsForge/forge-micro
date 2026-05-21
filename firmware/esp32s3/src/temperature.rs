//! BMM101 hashboard temperature sensor array on the shared I2C0 bus.
//!
//! The four-sensor logic lives in
//! [`ubos_temperature::lm75a::Lm75aHashboard`]; this module only binds it
//! to the board's concrete I2C bus type.

use embassy_embedded_hal::shared_bus::asynch::i2c::I2cDevice;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use esp_hal::i2c::master::I2c;

/// The I2C0 bus is shared between the PSU and temperature sensors via a
/// bus mutex; each consumer gets an `I2cDevice` handle from the mutex.
type I2cBus = I2cDevice<'static, NoopRawMutex, I2c<'static, esp_hal::Async>>;

pub type HashboardTemperature = ubos_temperature::lm75a::Lm75aHashboard<I2cBus>;
