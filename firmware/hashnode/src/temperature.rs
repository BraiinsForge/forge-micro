//! Hashnode board temperature sensing: TMP461 remote-diode (ASIC die)
//! plus two TMP102 PCB sensors on the shared I2C1 bus.
//!
//! The multi-sensor aggregation logic lives in
//! [`ubos_temperature::HashnodeTemperature`]; this module only binds it
//! to the board's concrete I2C bus type.

use embassy_embedded_hal::shared_bus::asynch::i2c::I2cDevice;
use embassy_rp::i2c::{Async, I2c};
use embassy_rp::peripherals::I2C1;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;

/// The I2C1 bus is shared between the PSU and temperature sensors via a
/// bus mutex; each consumer gets an `I2cDevice` handle from the mutex.
type I2cBus = I2cDevice<'static, NoopRawMutex, I2c<'static, I2C1, Async>>;

pub type HashboardTemperature = ubos_temperature::HashnodeTemperature<I2cBus>;
