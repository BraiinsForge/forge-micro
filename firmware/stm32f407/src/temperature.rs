//! BMM101 hashboard temperature sensor array on the shared I2C1 bus.
//!
//! The board's four LM75A sensors are declared as a `SensorDescriptor` table
//! and aggregated by `ubos_temperature::SensorArray`.

use embassy_embedded_hal::shared_bus::asynch::i2c::I2cDevice;
use embassy_stm32::i2c::{I2c, Master};
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use ubos_temperature::{SensorArray, SensorDescriptor, SensorKind};

/// The I2C1 bus is shared between the PSU and temperature sensors via a
/// bus mutex; each consumer gets an `I2cDevice` handle from the mutex.
type I2cBus = I2cDevice<'static, NoopRawMutex, I2c<'static, embassy_stm32::mode::Async, Master>>;

/// LM75A I2C addresses on the BMM101: IC12D, IC12C, IC12B, IC12A.
/// Each reading is offset by 7 °C to approximate chip junction temperature.
const SENSORS: [SensorDescriptor; 4] = [
    SensorDescriptor {
        kind: SensorKind::Lm75a,
        address: 0x49,
        pcb_to_die_offset_mc: 7_000,
        attached_chip: None,
    },
    SensorDescriptor {
        kind: SensorKind::Lm75a,
        address: 0x4a,
        pcb_to_die_offset_mc: 7_000,
        attached_chip: None,
    },
    SensorDescriptor {
        kind: SensorKind::Lm75a,
        address: 0x4b,
        pcb_to_die_offset_mc: 7_000,
        attached_chip: None,
    },
    SensorDescriptor {
        kind: SensorKind::Lm75a,
        address: 0x4c,
        pcb_to_die_offset_mc: 7_000,
        attached_chip: None,
    },
];

pub type HashboardTemperature = SensorArray<4, I2cBus>;

/// Construct the hashboard temperature aggregator on the shared I2C bus.
#[must_use]
pub fn new(bus: I2cBus) -> HashboardTemperature {
    SensorArray::new(bus, SENSORS)
}
