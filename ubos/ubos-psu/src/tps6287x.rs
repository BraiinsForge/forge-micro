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

//! `TPS6287x` DC/DC converter register layout and bus operations.
//!
//! Every operation here takes the bus as `&mut I` and the board's
//! descriptor as `&BmmPsuDescriptor`, rather than hanging off a driver
//! handle. `I` is a generic parameter, so no inherent `impl` in this
//! crate can name it, and the caller in `bmm.rs` has already locked its
//! `State` and drives this converter alongside an `INA230` and a
//! heartbeat part over the same bus. A method that locked again would
//! deadlock. `state.rs` holds the receiver these would otherwise want.

use ubos_core::units::Voltage;

#[cfg(test)]
#[path = "tests/tps6287x.rs"]
mod tests;

pub(crate) const REG_VSET: u8 = 0x00;
pub(crate) const REG_CONTROL1: u8 = 0x01;
pub(crate) const REG_CONTROL2: u8 = 0x02;
pub(crate) const REG_STATUS: u8 = 0x04;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum VoltageRampSpeed {
    VRamp10,
    VRamp5,
    #[default]
    VRamp1_25,
    VRamp0_5,
}

impl VoltageRampSpeed {
    const fn bits(self) -> u8 {
        match self {
            VoltageRampSpeed::VRamp10 => 0b00,
            VoltageRampSpeed::VRamp5 => 0b01,
            VoltageRampSpeed::VRamp1_25 => 0b10,
            VoltageRampSpeed::VRamp0_5 => 0b11,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SoftStartRampTime {
    SSTime0_5,
    #[default]
    SSTime1,
    SSTime2,
    SSTime4,
}

impl SoftStartRampTime {
    const fn bits(self) -> u8 {
        match self {
            SoftStartRampTime::SSTime0_5 => 0b00,
            SoftStartRampTime::SSTime1 => 0b01,
            SoftStartRampTime::SSTime2 => 0b10,
            SoftStartRampTime::SSTime4 => 0b11,
        }
    }

    pub(crate) const fn duration(self) -> embassy_time::Duration {
        match self {
            SoftStartRampTime::SSTime0_5 => embassy_time::Duration::from_micros(500),
            SoftStartRampTime::SSTime1 => embassy_time::Duration::from_millis(1),
            SoftStartRampTime::SSTime2 => embassy_time::Duration::from_millis(2),
            SoftStartRampTime::SSTime4 => embassy_time::Duration::from_millis(4),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum VoltageRange {
    /// 400–718 mV in 1.25 mV steps.
    VRange0,
    /// 400–1037 mV in 2.5 mV steps.
    VRange1,
    /// 400–1675 mV in 5 mV steps.
    #[default]
    VRange2,
    /// 800–3350 mV in 10 mV steps.
    VRange3,
}

impl VoltageRange {
    /// The lowest voltage the range reaches.
    pub const fn start(self) -> Voltage {
        match self {
            VoltageRange::VRange0 | VoltageRange::VRange1 | VoltageRange::VRange2 => {
                Voltage::from_mv(400)
            }
            VoltageRange::VRange3 => Voltage::from_mv(800),
        }
    }

    pub const fn step_microvolts(self) -> u32 {
        match self {
            VoltageRange::VRange0 => 1_250,
            VoltageRange::VRange1 => 2_500,
            VoltageRange::VRange2 => 5_000,
            VoltageRange::VRange3 => 10_000,
        }
    }

    /// The highest voltage the range reaches, rounded down.
    pub const fn end(self) -> Voltage {
        // 255 * step_microvolts / 1000, rounded down.
        // VRange2: 400 + 255*5 = 1675 mV.
        // VRange0: 400 + 255*1250/1000 = 400 + 318 = 718 mV.
        Voltage::from_mv(self.start().mv() + (255 * self.step_microvolts()) / 1_000)
    }

    /// Convert a voltage to a DAC value, rounding to the nearest
    /// step. Returns `None` if `voltage` is outside the supported
    /// range.
    pub fn voltage_to_dac(self, voltage: Voltage) -> Option<u8> {
        if voltage < self.start() {
            return None;
        }
        let microvolts_above = (voltage.mv() - self.start().mv()).checked_mul(1_000)?;
        let half_step = self.step_microvolts() / 2;
        let dac = (microvolts_above + half_step) / self.step_microvolts();
        if dac > 255 {
            return None;
        }
        Some(dac as u8)
    }

    /// The voltage a DAC value produces, rounded to the nearest
    /// millivolt.
    pub const fn dac_to_voltage(self, dac: u8) -> Voltage {
        const MICROVOLTS_PER_MILLIVOLT: u32 = 1_000;
        let microvolts_above = dac as u32 * self.step_microvolts();
        Voltage::from_mv(
            self.start().mv()
                + (microvolts_above + MICROVOLTS_PER_MILLIVOLT / 2) / MICROVOLTS_PER_MILLIVOLT,
        )
    }

    const fn bits(self) -> u8 {
        match self {
            VoltageRange::VRange0 => 0b00,
            VoltageRange::VRange1 => 0b01,
            VoltageRange::VRange2 => 0b10,
            VoltageRange::VRange3 => 0b11,
        }
    }
}

/// VSET register write for a DAC code.
///
/// The code is what the caller's voltage range maps a millivolt target
/// to, so this performs no range check of its own.
pub(crate) fn encode_vset(dac: u8) -> [u8; 2] {
    [REG_VSET, dac]
}

/// CONTROL1 with `dischen=1` (output discharge enabled to match
/// BOS default), optional `swen`, and the configured ramp speed.
pub(crate) fn encode_control1(swen: bool, vramp: VoltageRampSpeed) -> [u8; 2] {
    let mut value = 0u8;
    if swen {
        value |= 1 << 5;
    }
    value |= 1 << 3; // dischen
    value |= vramp.bits();
    [REG_CONTROL1, value]
}

/// CONTROL1 reset pulse — bit 7 set, all other fields zero.
/// Hardware clears the bit after self-reset completes.
pub(crate) fn encode_control1_reset() -> [u8; 2] {
    [REG_CONTROL1, 1 << 7]
}

/// CONTROL2 register write carrying the voltage range and soft-start
/// ramp time.
///
/// The voltage range sits in bits 3:2 and the ramp time in bits 1:0.
/// Changing the range rescales every DAC code, so this is written during
/// init and not again.
pub(crate) fn encode_control2(vrange: VoltageRange, sstime: SoftStartRampTime) -> [u8; 2] {
    let value = (vrange.bits() << 2) | sstime.bits();
    [REG_CONTROL2, value]
}

/// Decode this part's STATUS byte into the shared [`crate::StatusFlags`].
///
/// The bit layout is this converter's own and is not the `PMBus`
/// `STATUS_BYTE` that `sic45x.rs` and `tps53647.rs` decode: overvoltage
/// is bit 0, undervoltage bit 1, thermal shutdown bit 2 and current
/// limit bit 4. The two decoders look interchangeable and are not.
pub(crate) fn decode_status(byte: u8) -> crate::StatusFlags {
    crate::StatusFlags {
        undervoltage: (byte & 0b0000_0010) != 0,
        overvoltage: (byte & 0b0000_0001) != 0,
        thermal_shutdown: (byte & 0b0000_0100) != 0,
        current_limit: (byte & 0b0001_0000) != 0,
    }
}

use embassy_time::{Duration, Timer};
use embedded_hal_async::i2c::I2c;

use crate::{BmmPsuDescriptor, Error};

/// `t_d(EN)2` = 100 µs after the reset pulse before further writes.
const SW_ENABLE_DELAY: Duration = Duration::from_micros(100);

/// Reset the converter, program its range and ramp, and set the initial
/// output voltage.
///
/// The output stays off. `enable` is what starts switching, so a board
/// can program a rail before it energises it.
///
/// # Errors
///
/// Returns [`Error::OutOfRange`] when the descriptor's initial voltage
/// falls outside its configured range, and [`Error::Bus`] on any I2C
/// failure.
pub(crate) async fn init<I, E>(i2c: &mut I, config: &BmmPsuDescriptor) -> Result<(), Error<E>>
where
    I: I2c<Error = E>,
    E: embedded_hal_async::i2c::Error,
{
    reset(i2c, config).await?;
    i2c.write(
        config.dcdc_address,
        &encode_control2(config.dcdc_voltage_range, config.dcdc_soft_start),
    )
    .await
    .map_err(Error::Bus)?;
    let dac = config
        .dcdc_voltage_range
        .voltage_to_dac(config.dcdc_initial)
        .ok_or(Error::OutOfRange)?;
    i2c.write(config.dcdc_address, &encode_vset(dac))
        .await
        .map_err(Error::Bus)
}

/// Pulse the converter's software reset, then restore CONTROL1 with
/// switching off.
///
/// The wait between the two writes is [`SW_ENABLE_DELAY`], which the
/// part needs before it accepts another write. Skipping it drops the
/// second write silently.
///
/// # Errors
///
/// Returns [`Error::Bus`] on any I2C failure.
async fn reset<I, E>(i2c: &mut I, config: &BmmPsuDescriptor) -> Result<(), Error<E>>
where
    I: I2c<Error = E>,
    E: embedded_hal_async::i2c::Error,
{
    i2c.write(config.dcdc_address, &encode_control1_reset())
        .await
        .map_err(Error::Bus)?;
    Timer::after(SW_ENABLE_DELAY).await;
    i2c.write(
        config.dcdc_address,
        &encode_control1(false, config.dcdc_ramp_speed),
    )
    .await
    .map_err(Error::Bus)
}

/// Start switching and wait out the soft-start ramp.
///
/// Returns once the rail has had its configured soft-start time to come
/// up, so a caller can read the output straight after. The STATUS read
/// at the end is deliberate and its result is dropped: it clears the
/// undervoltage bit that latches while the rail is still ramping. A real
/// fault latches again and surfaces on the next `read_status`.
///
/// # Errors
///
/// Returns [`Error::Bus`] when the CONTROL1 write fails. A failure of
/// the STATUS read is swallowed, because by then the rail is up.
pub(crate) async fn enable<I, E>(i2c: &mut I, config: &BmmPsuDescriptor) -> Result<(), Error<E>>
where
    I: I2c<Error = E>,
    E: embedded_hal_async::i2c::Error,
{
    i2c.write(
        config.dcdc_address,
        &encode_control1(true, config.dcdc_ramp_speed),
    )
    .await
    .map_err(Error::Bus)?;
    Timer::after(config.dcdc_soft_start.duration()).await;
    // Read STATUS to clear bits latched during the soft-start
    // ramp (undervoltage is transient until the rail settles).
    // Real faults surface on the next user call; matches BOS.
    let _ = read_status(i2c, config).await;
    Ok(())
}

/// Stop switching, leaving the programmed voltage in place.
///
/// Output discharge stays enabled, so the rail is pulled down rather
/// than left to decay through whatever load is on it.
///
/// # Errors
///
/// Returns [`Error::Bus`] on any I2C failure.
pub(crate) async fn disable<I, E>(i2c: &mut I, config: &BmmPsuDescriptor) -> Result<(), Error<E>>
where
    I: I2c<Error = E>,
    E: embedded_hal_async::i2c::Error,
{
    i2c.write(
        config.dcdc_address,
        &encode_control1(false, config.dcdc_ramp_speed),
    )
    .await
    .map_err(Error::Bus)
}

/// Command a new output voltage.
///
/// The range programmed at init fixes what is reachable, and a target
/// outside it is rejected before the bus is touched.
///
/// # Errors
///
/// Returns [`Error::OutOfRange`] when `voltage` does not map to a DAC
/// code in the configured range, and [`Error::Bus`] on I2C failure.
pub(crate) async fn set_voltage<I, E>(
    i2c: &mut I,
    config: &BmmPsuDescriptor,
    voltage: Voltage,
) -> Result<Voltage, Error<E>>
where
    I: I2c<Error = E>,
    E: embedded_hal_async::i2c::Error,
{
    let dac = config
        .dcdc_voltage_range
        .voltage_to_dac(voltage)
        .ok_or(Error::OutOfRange)?;
    i2c.write(config.dcdc_address, &encode_vset(dac))
        .await
        .map_err(Error::Bus)?;
    Ok(config.dcdc_voltage_range.dac_to_voltage(dac))
}

/// Read the STATUS register and decode the four shared fault flags.
///
/// Reading clears the latched bits, so two calls in a row report
/// different things and the second is not the healthier answer.
///
/// # Errors
///
/// Returns [`Error::Bus`] on any I2C failure.
pub(crate) async fn read_status<I, E>(
    i2c: &mut I,
    config: &BmmPsuDescriptor,
) -> Result<crate::StatusFlags, Error<E>>
where
    I: I2c<Error = E>,
    E: embedded_hal_async::i2c::Error,
{
    let mut buffer = [0u8; 1];
    i2c.write_read(config.dcdc_address, &[REG_STATUS], &mut buffer)
        .await
        .map_err(Error::Bus)?;
    Ok(decode_status(buffer[0]))
}
