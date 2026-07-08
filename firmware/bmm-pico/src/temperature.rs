//! BMM101 hashboard temperature sensor array on the shared I2C1 bus.
//!
//! The board's LM75A sensors come from the machine descriptor's sensor
//! table and are aggregated by `ubos_temperature::SensorArray`.

use embassy_embedded_hal::shared_bus::asynch::i2c::I2cDevice;
use embassy_rp::i2c::{Async, I2c};
use embassy_rp::peripherals::I2C1;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use ubos_machine::MINIMINER_BMM_ADAPTER as MACHINE;
use ubos_temperature::SensorArray;

/// The I2C1 bus is shared between the PSU and temperature sensors via a
/// bus mutex; each consumer gets an `I2cDevice` handle from the mutex.
type I2cBus = I2cDevice<'static, NoopRawMutex, I2c<'static, I2C1, Async>>;

pub type HashboardTemperature = SensorArray<I2cBus>;

/// Construct the hashboard temperature aggregator on the shared I2C bus.
#[must_use]
pub fn new(bus: I2cBus) -> HashboardTemperature {
    SensorArray::new(bus, MACHINE.temp)
}
