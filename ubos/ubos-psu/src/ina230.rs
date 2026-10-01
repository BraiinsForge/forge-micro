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

//! `INA230` high-side current/power monitor.
//!
//! Every function here takes the bus as `&mut I` rather than a method on
//! a handle, for the same reason as [`crate::sic45x`]: see its module
//! doc.
//!
//! Calibration math operates in integer microamps and microohms:
//! `current_lsb_microamps = max_current_ma * 1_000 / 32_768`
//! `cal_value = 5_120_000_000 / (current_lsb_microamps * shunt_microohm)`
//! `power_mw = power_register * 25 * current_lsb_microamps / 1_000_000`

use embedded_hal_async::i2c::I2c;
use ubos_core::units::{Current, Power, Voltage};

#[cfg(test)]
#[path = "tests/ina230.rs"]
mod tests;

const REG_CONFIG: u8 = 0x00;
const REG_BUS_VOLTAGE: u8 = 0x02;
const REG_POWER: u8 = 0x03;
const REG_CALIBRATION: u8 = 0x05;

/// Bus-voltage LSB in microvolts: INA230 reports the rail voltage in
/// 1.25 mV steps.
const BUS_VOLTAGE_LSB_MICROVOLTS: u32 = 1_250;

/// CONFIG: averaging=Avg1, `VBus` CT=1.1ms, Shunt CT=1.1ms, Mode=continuous
/// shunt+bus.
const CONFIG_VALUE: u16 = 0b0100_0001_0010_0111;

/// Current LSB in microamps for a full-scale current of `max_current`.
///
/// The current register is signed 16-bit, so one LSB is the full-scale
/// current divided by 2^15. Every other scaling here derives from this
/// one value.
pub(crate) const fn current_lsb_microamps(max_current: Current) -> u32 {
    max_current.ma() * 1_000 / 32_768
}

/// CALIBRATION register value for a current LSB and a shunt resistance.
///
/// Both inputs are in micro-units, and the constant folds the 5.12e-3
/// calibration term and the two 1e-6 scalings into one integer numerator
/// so the whole calculation stays in `u64` with no floating point.
///
/// Saturates at `u16::MAX` rather than wrapping. A shunt small enough to
/// overflow this would be a descriptor error, and a saturated
/// calibration reads high rather than reporting a wrapped small number
/// as a healthy one.
pub(crate) const fn calibration_value(current_lsb_microamps: u32, shunt_microohm: u32) -> u16 {
    // 5.12e-3 / (lsb_µA * 1e-6 * shunt_µΩ * 1e-6)
    //   = 5.12e9 / (lsb_µA * shunt_µΩ)
    let numerator: u64 = 5_120_000_000;
    let denominator: u64 = current_lsb_microamps as u64 * shunt_microohm as u64;
    let cal = numerator / denominator;
    if cal > u16::MAX as u64 {
        u16::MAX
    } else {
        cal as u16
    }
}

/// A raw POWER register reading with the full-scale current that
/// calibrated it, because the register alone does not give the power.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RawPower {
    pub(crate) register: u16,
    pub(crate) max_current: Current,
}

/// The power LSB is fixed at 25 times the current LSB, so the result is
/// `register * 25 * current_lsb_microamps` scaled from microwatts. The
/// intermediate is `u64`, because the microwatt product overflows `u32`
/// at the currents this part measures.
impl From<RawPower> for Power {
    fn from(raw: RawPower) -> Self {
        // power_lsb_microwatts = 25 * current_lsb_microamps
        // power_milliwatts = power_register * power_lsb_microwatts / 1_000
        let microwatts =
            u64::from(raw.register) * 25 * u64::from(current_lsb_microamps(raw.max_current));
        Self::from_mw((microwatts / 1_000) as u32)
    }
}

/// A raw bus-voltage register reading.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RawBusVoltage(pub(crate) u16);

/// The register is unsigned 16-bit with a 1.25 mV LSB, so the value
/// fits in `u32` for any rail this driver targets.
impl From<RawBusVoltage> for Voltage {
    fn from(raw: RawBusVoltage) -> Self {
        Self::from_mv((u32::from(raw.0) * BUS_VOLTAGE_LSB_MICROVOLTS) / 1_000)
    }
}

/// Program the CONFIG and CALIBRATION registers.
///
/// Runs once per boot, before any read. The part measures nothing useful
/// until CALIBRATION is set, so a caller that skips this reads zeros
/// rather than an error.
///
/// # Errors
///
/// Returns the bus error when either write fails.
pub(crate) async fn init<I, E>(
    i2c: &mut I,
    address: u8,
    max_current: Current,
    shunt_microohm: u32,
) -> Result<(), E>
where
    I: I2c<Error = E>,
{
    let mut config_buffer = [0u8; 3];
    config_buffer[0] = REG_CONFIG;
    config_buffer[1..].copy_from_slice(&CONFIG_VALUE.to_be_bytes());
    i2c.write(address, &config_buffer).await?;

    let cal = calibration_value(current_lsb_microamps(max_current), shunt_microohm);
    let mut cal_buffer = [0u8; 3];
    cal_buffer[0] = REG_CALIBRATION;
    cal_buffer[1..].copy_from_slice(&cal.to_be_bytes());
    i2c.write(address, &cal_buffer).await
}

/// Read the POWER register.
///
/// `max_current` must match what [`init`] was given, because the
/// scaling is derived from it rather than read back from the part.
///
/// # Errors
///
/// Returns the bus error when the read fails.
pub(crate) async fn read_power<I, E>(
    i2c: &mut I,
    address: u8,
    max_current: Current,
) -> Result<Power, E>
where
    I: I2c<Error = E>,
{
    let mut buffer = [0u8; 2];
    i2c.write_read(address, &[REG_POWER], &mut buffer).await?;
    let power_register = u16::from_be_bytes(buffer);
    let power: Power = RawPower {
        register: power_register,
        max_current,
    }
    .into();
    #[cfg(feature = "defmt")]
    ubos_logging::trace!(
        "INA230 power @{:x}: raw=[{:x} {:x}] reg={:x} -> {} mW",
        address,
        buffer[0],
        buffer[1],
        power_register,
        power.mw(),
    );
    Ok(power)
}

/// Read the bus-voltage register.
pub(crate) async fn read_bus_voltage<I, E>(i2c: &mut I, address: u8) -> Result<Voltage, E>
where
    I: I2c<Error = E>,
{
    let mut buffer = [0u8; 2];
    i2c.write_read(address, &[REG_BUS_VOLTAGE], &mut buffer)
        .await?;
    let register = u16::from_be_bytes(buffer);
    Ok(RawBusVoltage(register).into())
}
