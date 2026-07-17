//! Braiins Axe board temperature: two TMP102 PCB sensors on the shared
//! I2C0 bus, aggregated by `ubos_temperature::SensorArray` from the
//! `ubos_machine::BRAIINS_AXE` sensor table. There is no die-attached remote
//! diode on this board, so the safety gate runs on the PCB readings
//! plus their PCB-to-die offset.

use embassy_embedded_hal::shared_bus::asynch::i2c::I2cDevice;
use embassy_rp::i2c::{Async, I2c};
use embassy_rp::peripherals::I2C0;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use ubos_machine::BRAIINS_AXE as MACHINE;
use ubos_temperature::SensorArray;

/// The I2C0 bus is shared between the PSU and temperature sensors via a
/// bus mutex; each consumer gets an `I2cDevice` handle from the mutex.
type I2cBus = I2cDevice<'static, NoopRawMutex, I2c<'static, I2C0, Async>>;

pub type HashboardTemperature = SensorArray<I2cBus>;

/// Construct the hashboard temperature aggregator on the shared I2C0 bus.
#[must_use]
pub fn new(bus: I2cBus) -> HashboardTemperature {
    SensorArray::new(bus, MACHINE.temp)
}
