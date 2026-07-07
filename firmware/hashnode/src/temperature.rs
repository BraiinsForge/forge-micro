//! Hashnode board temperature: a TMP461 remote-diode sensor on the ASIC
//! die plus two TMP102 PCB sensors on the shared I2C1 bus, aggregated by
//! `ubos_temperature::SensorArray` from the `ubos_machine::HASHNODE`
//! sensor table.
//!
//! Note on `dangerous_temp_c`: the 90 °C runtime default in `ThermalConfig`
//! is calibrated for PCB-surface sensors. Because the safety gate reads the
//! real die temperature from the TMP461 remote channel, the Hashnode
//! threshold should be raised toward ~108 °C; override it in the per-board
//! `ThermalConfig`, not in the shared `ubos-config` default.

use embassy_embedded_hal::shared_bus::asynch::i2c::I2cDevice;
use embassy_rp::i2c::{Async, I2c};
use embassy_rp::peripherals::I2C1;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use ubos_machine::HASHNODE as MACHINE;
use ubos_temperature::SensorArray;

/// The I2C1 bus is shared between the PSU and temperature sensors via a
/// bus mutex; each consumer gets an `I2cDevice` handle from the mutex.
type I2cBus = I2cDevice<'static, NoopRawMutex, I2c<'static, I2C1, Async>>;

pub type HashboardTemperature = SensorArray<I2cBus>;

/// Construct the hashboard temperature aggregator on the shared I2C1 bus.
#[must_use]
pub fn new(bus: I2cBus) -> HashboardTemperature {
    SensorArray::new(bus, MACHINE.temp)
}
