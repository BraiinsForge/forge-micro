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

//! Driver for the Vishay `SIC45x` `PMBus` DC/DC converter.
//!
//! All configuration is in typed units. The chip address is
//! supplied by the caller — this module is address-parameterized.
//!
//! Every operation here takes the bus as `&mut I` and the device config
//! by reference, rather than hanging off a driver handle. Two things fix
//! that. `I` is a generic parameter, so no inherent `impl` in this crate
//! can name it. And the caller is a PSU handle that has already locked
//! its `State` for the whole operation, so a method that locked again
//! would deadlock.
//!
//! The receiver these would otherwise want already exists one layer up:
//! `State<I, C>` in `state.rs` holds both the bus and the config, and
//! the handle types wrap it. A newtype over the bus alone would not be
//! it, because the config still travels by hand.

use embedded_hal_async::i2c::I2c;
use ubos_core::units::{Current, Power, Temperature, Voltage};

use crate::Error;
use crate::pmbus::{decode_status, write_word};

#[cfg(test)]
#[path = "tests/sic45x.rs"]
mod tests;

// Register addresses (PMBus command codes).
pub(crate) const OPERATION: u8 = 0x01;
pub(crate) const CLEAR_FAULTS: u8 = 0x03;
pub(crate) const RESTORE_USER_ALL: u8 = 0x16;
pub(crate) const VOUT_COMMAND: u8 = 0x21;
pub(crate) const VOUT_SCALE_LOOP: u8 = 0x29;
pub(crate) const VOUT_OV_FAULT_LIMIT: u8 = 0x40;
pub(crate) const VOUT_OV_FAULT_RESPONSE: u8 = 0x41;
pub(crate) const VOUT_UV_FAULT_LIMIT: u8 = 0x44;
pub(crate) const VOUT_UV_FAULT_RESPONSE: u8 = 0x45;
pub(crate) const IOUT_OC_FAULT_LIMIT: u8 = 0x46;
pub(crate) const IOUT_OC_FAULT_RESPONSE: u8 = 0x47;
pub(crate) const IOUT_OC_WARN_LIMIT: u8 = 0x4A;
pub(crate) const OT_FAULT_LIMIT: u8 = 0x4F;
pub(crate) const OT_FAULT_RESPONSE: u8 = 0x50;
pub(crate) const OT_WARN_LIMIT: u8 = 0x51;
pub(crate) const VIN_OV_FAULT_LIMIT: u8 = 0x55;
pub(crate) const VIN_OV_FAULT_RESPONSE: u8 = 0x56;
pub(crate) const VIN_UV_WARN_LIMIT: u8 = 0x58;
pub(crate) const IIN_OC_WARN_LIMIT: u8 = 0x5D;
pub(crate) const POWER_GOOD_ON: u8 = 0x5E;
pub(crate) const POWER_GOOD_OFF: u8 = 0x5F;
pub(crate) const STATUS_BYTE: u8 = 0x78;
pub(crate) const READ_VOUT: u8 = 0x8B;
pub(crate) const READ_POUT: u8 = 0x96;
pub(crate) const IC_DEVICE_ID: u8 = 0xAD;

/// OPERATION register on-bit: sets the output enabled state.
const OPERATION_ON: u8 = 1 << 7;

// Fault-response defaults from datasheet / BOS reference design.
const VOUT_OV_FAULT_RESPONSE_DEFAULT: u8 = 0xF8;
const VOUT_UV_FAULT_RESPONSE_DEFAULT: u8 = 0xB9;
const IOUT_OC_FAULT_RESPONSE_DEFAULT: u8 = 0xA1;
const OT_FAULT_RESPONSE_DEFAULT: u8 = 0xF9;
const VIN_OV_FAULT_RESPONSE_DEFAULT: u8 = 0xB8;

/// `VOUT_SCALE_LOOP` setting. Controls the converter's output voltage
/// feedback divider scale factor. Select to match your hardware
/// resistor divider ratio.
///
/// Each variant's name is the LINEAR11-decoded value of its register:
/// exponent field `0b11101` = −3, so the scale = mantissa × 2^−3.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LoopScale {
    /// Scale factor 0.125 (1/8): mantissa 1, register `0xE801`.
    Scale0_125,
    /// Scale factor 0.25 (1/4): mantissa 2, register `0xE802`.
    Scale0_25,
    /// Scale factor 0.5 (1/2): mantissa 4, register `0xE804`.
    #[default]
    Scale0_5,
    /// Scale factor 1.0 (unity): mantissa 8, register `0xE808`.
    Scale1_0,
}

impl LoopScale {
    /// Return the 16-bit register value for this scale setting.
    pub const fn register(self) -> u16 {
        match self {
            LoopScale::Scale0_125 => 0xE801,
            LoopScale::Scale0_25 => 0xE802,
            LoopScale::Scale0_5 => 0xE804,
            LoopScale::Scale1_0 => 0xE808,
        }
    }
}

/// Encode a VOUT voltage to the ULINEAR16 register format.
///
/// LSB = 1/512 V. Rounds to nearest representable value.
pub(crate) const fn encode_vout(voltage: Voltage) -> u16 {
    ((voltage.mv() * 512 + 500) / 1000) as u16
}

/// A raw `READ_VOUT` word in the ULINEAR16 register format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RawVout(pub(crate) u16);

/// Inverse of [`encode_vout`]: LSB = 1/512 V, rounded to nearest mV.
impl From<RawVout> for Voltage {
    fn from(raw: RawVout) -> Self {
        Self::from_mv((u32::from(raw.0) * 1000 + 256) / 512)
    }
}

/// Encode a value in thousandths of a unit to LINEAR11 format with
/// exponent −1, so the LSB is 0.5 units.
const fn encode_lin11_half(milli: u32) -> u16 {
    // Exponent field -1 = 0x1F in 5-bit two's complement → upper 5
    // bits = 11111 = 0xF8xx. Mantissa = milli / 0.5 = milli*2/1000,
    // rounded to nearest: (milli + 250) / 500.
    0xF800 | (((milli + 250) / 500) as u16)
}

/// Encode a VIN limit to LINEAR11 format with exponent −1.
pub(crate) const fn encode_lin11_half_voltage(voltage: Voltage) -> u16 {
    encode_lin11_half(voltage.mv())
}

/// Encode an IOUT or IIN limit to LINEAR11 format with exponent −1.
pub(crate) const fn encode_lin11_half_current(current: Current) -> u16 {
    encode_lin11_half(current.ma())
}

/// Encode a temperature limit in whole degrees Celsius to LINEAR11
/// format with exponent 0 (LSB = 1 °C).
pub(crate) const fn encode_temp(temperature: Temperature) -> u16 {
    // Temperature limits are whole degrees well under u16::MAX, so the
    // truncation never loses significant bits.
    temperature.celsius_i32().cast_unsigned() as u16
}

/// A raw `ULINEAR11` word, as `READ_POUT` and `READ_PIN` return it.
///
/// It has an unsigned 11-bit mantissa with a signed 5-bit two's-complement
/// exponent (per the `SIC45x` / `PMBus` spec), unlike the signed-mantissa
/// `LINEAR11` used for VIN/IOUT.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RawUlinear11(pub(crate) u16);

/// Decode a `ULINEAR11` register value to milli-units.
///
/// Used for a power reading, so a negative result is residual noise
/// around zero load and [`From<RawUlinear11>`] clamps it to zero.
const fn decode_ulin11_milli(raw: u16) -> i32 {
    let mantissa = (raw & 0x07FF) as i64;
    let exponent_field = (raw >> 11) as i64;
    let exponent = if exponent_field & 0x10 != 0 {
        exponent_field - 0x20
    } else {
        exponent_field
    };
    let milli = mantissa * 1000;
    let result = if exponent >= 0 {
        milli << exponent
    } else {
        milli >> (-exponent)
    };
    if result > i32::MAX as i64 {
        i32::MAX
    } else if result < i32::MIN as i64 {
        i32::MIN
    } else {
        result as i32
    }
}

impl From<RawUlinear11> for Power {
    fn from(raw: RawUlinear11) -> Self {
        Self::from_mw(decode_ulin11_milli(raw.0).max(0).cast_unsigned())
    }
}

/// Configuration for the `SIC45x` `PMBus` DC/DC converter.
///
/// All fields are typed units. Embed this in a board
/// config struct; pass a reference to every driver function.
#[derive(Clone, Copy, Debug)]
pub struct Sic45xConfig {
    /// `PMBus` 7-bit address, selected by hardware resistor.
    pub address: u8,
    /// Output voltage to program during init.
    pub initial_voltage: Voltage,
    /// Feedback divider scale to match the board's resistor network.
    pub loop_scale: LoopScale,
    /// VOUT overvoltage fault threshold.
    pub vout_ov_fault: Voltage,
    /// VOUT undervoltage fault threshold.
    pub vout_uv_fault: Voltage,
    /// VIN overvoltage fault threshold.
    pub vin_ov_fault: Voltage,
    /// VIN undervoltage warning threshold.
    pub vin_uv_warn: Voltage,
    /// IOUT overcurrent fault threshold.
    pub iout_oc_fault: Current,
    /// IOUT overcurrent warning threshold.
    pub iout_oc_warn: Current,
    /// IIN overcurrent warning threshold. The device has no IIN fault
    /// register, so input overcurrent only raises a warning.
    pub iin_oc_warn: Current,
    /// Overtemperature fault threshold.
    pub ot_fault: Temperature,
    /// Overtemperature warning threshold.
    pub ot_warn: Temperature,
    /// `POWER_GOOD` asserts when VOUT rises above this threshold.
    pub power_good_on: Voltage,
    /// `POWER_GOOD` deasserts when VOUT falls below this threshold.
    pub power_good_off: Voltage,
    /// Minimum voltage accepted by [`set_voltage`].
    pub voltage_min: Voltage,
    /// Maximum voltage accepted by [`set_voltage`].
    pub voltage_max: Voltage,
}

impl Sic45xConfig {
    /// Default `SIC45x` configuration. The operating-point values are
    /// taken from the Femto miner.
    pub const DEFAULT: Self = Self {
        address: 0x10,
        initial_voltage: Voltage::from_mv(600),
        loop_scale: LoopScale::Scale0_5,
        vout_ov_fault: Voltage::from_mv(2650),
        vout_uv_fault: Voltage::from_mv(350),
        vin_ov_fault: Voltage::from_mv(18_000),
        vin_uv_warn: Voltage::from_mv(9_000),
        iout_oc_fault: Current::from_ma(25_000),
        iout_oc_warn: Current::from_ma(20_000),
        iin_oc_warn: Current::from_ma(5_000),
        ot_fault: Temperature::from_celsius(115),
        ot_warn: Temperature::from_celsius(95),
        power_good_on: Voltage::from_mv(540),
        power_good_off: Voltage::from_mv(510),
        voltage_min: Voltage::from_mv(400),
        voltage_max: Voltage::from_mv(2400),
    };
}

/// Issue the full power-up register write sequence to the `SIC45x`.
///
/// Programs all protection limits, fault responses, and the initial
/// output voltage. Does not touch `OPERATION`: the rail is gated by the
/// board's hardware enable pin, and the output is already off during
/// programming.
///
/// # Errors
///
/// Returns [`Error::Bus`] on any I2C transaction failure.
// The function is a flat sequence of PMBus writes; splitting it would add
// indirection without reducing complexity.
#[allow(clippy::too_many_lines)]
pub async fn init<I, E>(i2c: &mut I, config: &Sic45xConfig) -> Result<(), Error<E>>
where
    I: I2c<Error = E>,
    E: embedded_hal_async::i2c::Error,
{
    let address = config.address;

    // Restore user defaults so OTP values baseline all registers
    // before we write our limits — matches BOS bring-up sequence.
    i2c.write(address, &[RESTORE_USER_ALL])
        .await
        .map_err(Error::Bus)?;

    // No OPERATION write here. The output must stay off while registers are
    // being programmed: RESTORE_USER_ALL clears the on-bit and the SIC_EN
    // GPIO is held low by the caller. OPERATION is asserted later by enable(),
    // once all protection limits are in place.
    i2c.write(address, &[CLEAR_FAULTS])
        .await
        .map_err(Error::Bus)?;

    write_word(i2c, address, VOUT_SCALE_LOOP, config.loop_scale.register()).await?;
    write_word(
        i2c,
        address,
        VOUT_OV_FAULT_LIMIT,
        encode_vout(config.vout_ov_fault),
    )
    .await?;
    write_word(
        i2c,
        address,
        VOUT_UV_FAULT_LIMIT,
        encode_vout(config.vout_uv_fault),
    )
    .await?;
    write_word(
        i2c,
        address,
        VIN_OV_FAULT_LIMIT,
        encode_lin11_half_voltage(config.vin_ov_fault),
    )
    .await?;
    write_word(
        i2c,
        address,
        IOUT_OC_FAULT_LIMIT,
        encode_lin11_half_current(config.iout_oc_fault),
    )
    .await?;
    write_word(i2c, address, OT_FAULT_LIMIT, encode_temp(config.ot_fault)).await?;
    write_word(
        i2c,
        address,
        VIN_UV_WARN_LIMIT,
        encode_lin11_half_voltage(config.vin_uv_warn),
    )
    .await?;
    write_word(
        i2c,
        address,
        IOUT_OC_WARN_LIMIT,
        encode_lin11_half_current(config.iout_oc_warn),
    )
    .await?;
    write_word(
        i2c,
        address,
        IIN_OC_WARN_LIMIT,
        encode_lin11_half_current(config.iin_oc_warn),
    )
    .await?;
    write_word(i2c, address, OT_WARN_LIMIT, encode_temp(config.ot_warn)).await?;

    i2c.write(
        address,
        &[VOUT_OV_FAULT_RESPONSE, VOUT_OV_FAULT_RESPONSE_DEFAULT],
    )
    .await
    .map_err(Error::Bus)?;
    i2c.write(
        address,
        &[VOUT_UV_FAULT_RESPONSE, VOUT_UV_FAULT_RESPONSE_DEFAULT],
    )
    .await
    .map_err(Error::Bus)?;
    i2c.write(
        address,
        &[IOUT_OC_FAULT_RESPONSE, IOUT_OC_FAULT_RESPONSE_DEFAULT],
    )
    .await
    .map_err(Error::Bus)?;
    i2c.write(address, &[OT_FAULT_RESPONSE, OT_FAULT_RESPONSE_DEFAULT])
        .await
        .map_err(Error::Bus)?;
    i2c.write(
        address,
        &[VIN_OV_FAULT_RESPONSE, VIN_OV_FAULT_RESPONSE_DEFAULT],
    )
    .await
    .map_err(Error::Bus)?;

    write_word(
        i2c,
        address,
        POWER_GOOD_ON,
        encode_vout(config.power_good_on),
    )
    .await?;
    write_word(
        i2c,
        address,
        POWER_GOOD_OFF,
        encode_vout(config.power_good_off),
    )
    .await?;
    write_word(
        i2c,
        address,
        VOUT_COMMAND,
        encode_vout(config.initial_voltage),
    )
    .await?;

    i2c.write(address, &[CLEAR_FAULTS])
        .await
        .map_err(Error::Bus)
}

/// Enable the converter output by setting the OPERATION on-bit.
///
/// Reads the current OPERATION register value, ORs in bit 7, and
/// writes it back — matching the BOS read-modify-write sequence.
///
/// # Errors
///
/// Returns [`Error::Bus`] on any I2C transaction failure.
pub async fn enable<I, E>(i2c: &mut I, config: &Sic45xConfig) -> Result<(), Error<E>>
where
    I: I2c<Error = E>,
    E: embedded_hal_async::i2c::Error,
{
    let mut buffer = [0u8; 1];
    i2c.write_read(config.address, &[OPERATION], &mut buffer)
        .await
        .map_err(Error::Bus)?;
    let new_value = buffer[0] | OPERATION_ON;
    i2c.write(config.address, &[OPERATION, new_value])
        .await
        .map_err(Error::Bus)?;
    i2c.write(config.address, &[CLEAR_FAULTS])
        .await
        .map_err(Error::Bus)
}

/// Disable the converter output by clearing the OPERATION on-bit.
///
/// # Errors
///
/// Returns [`Error::Bus`] on any I2C transaction failure.
pub async fn disable<I, E>(i2c: &mut I, config: &Sic45xConfig) -> Result<(), Error<E>>
where
    I: I2c<Error = E>,
    E: embedded_hal_async::i2c::Error,
{
    let mut buffer = [0u8; 1];
    i2c.write_read(config.address, &[OPERATION], &mut buffer)
        .await
        .map_err(Error::Bus)?;
    let new_value = buffer[0] & !OPERATION_ON;
    i2c.write(config.address, &[OPERATION, new_value])
        .await
        .map_err(Error::Bus)
}

/// Program a new output voltage.
///
/// # Errors
///
/// Returns [`Error::OutOfRange`] without touching the bus if
/// `voltage` is outside the configured limits. Returns
/// [`Error::Bus`] on I2C failure.
pub async fn set_voltage<I, E>(
    i2c: &mut I,
    config: &Sic45xConfig,
    voltage: Voltage,
) -> Result<(), Error<E>>
where
    I: I2c<Error = E>,
    E: embedded_hal_async::i2c::Error,
{
    if voltage < config.voltage_min || voltage > config.voltage_max {
        return Err(Error::OutOfRange);
    }
    let bytes = encode_vout(voltage).to_le_bytes();
    i2c.write(config.address, &[VOUT_COMMAND, bytes[0], bytes[1]])
        .await
        .map_err(Error::Bus)
}

/// Read the output power via `READ_POUT`.
///
/// Negative decoded values (e.g. during initial ramp) are clamped
/// to zero.
///
/// # Errors
///
/// Returns [`Error::Bus`] on I2C failure.
pub async fn read_pout<I, E>(i2c: &mut I, config: &Sic45xConfig) -> Result<Power, Error<E>>
where
    I: I2c<Error = E>,
    E: embedded_hal_async::i2c::Error,
{
    let mut buffer = [0u8; 2];
    i2c.write_read(config.address, &[READ_POUT], &mut buffer)
        .await
        .map_err(Error::Bus)?;
    let raw = u16::from_le_bytes(buffer);
    let power: Power = RawUlinear11(raw).into();
    #[cfg(feature = "defmt")]
    ubos_logging::trace!(
        "SIC READ_POUT @{:x}: raw=[{:x} {:x}] word={:x} -> {} mW",
        config.address,
        buffer[0],
        buffer[1],
        raw,
        power.mw(),
    );
    Ok(power)
}

/// Read `READ_VOUT` and decode the actual output voltage.
///
/// Unlike the INA230 (powered from the output rail, asleep until it
/// comes up), the SIC reads its own VOUT from VIN and is available as
/// soon as input power is present.
///
/// # Errors
///
/// Returns [`Error::Bus`] on I2C failure.
pub async fn read_vout<I, E>(i2c: &mut I, config: &Sic45xConfig) -> Result<Voltage, Error<E>>
where
    I: I2c<Error = E>,
    E: embedded_hal_async::i2c::Error,
{
    let mut buffer = [0u8; 2];
    i2c.write_read(config.address, &[READ_VOUT], &mut buffer)
        .await
        .map_err(Error::Bus)?;
    let raw = u16::from_le_bytes(buffer);
    let voltage: Voltage = RawVout(raw).into();
    #[cfg(feature = "defmt")]
    ubos_logging::trace!(
        "SIC READ_VOUT @{:x}: raw=[{:x} {:x}] word={:x} -> {} mV",
        config.address,
        buffer[0],
        buffer[1],
        raw,
        voltage.mv(),
    );
    Ok(voltage)
}

/// Read `STATUS_BYTE` and decode into the shared [`crate::StatusFlags`] type.
///
/// # Errors
///
/// Returns [`Error::Bus`] on I2C failure.
pub async fn read_status<I, E>(
    i2c: &mut I,
    config: &Sic45xConfig,
) -> Result<crate::StatusFlags, Error<E>>
where
    I: I2c<Error = E>,
    E: embedded_hal_async::i2c::Error,
{
    let mut buffer = [0u8; 1];
    i2c.write_read(config.address, &[STATUS_BYTE], &mut buffer)
        .await
        .map_err(Error::Bus)?;
    Ok(decode_status(buffer[0]))
}

/// Read the `IC_DEVICE_ID` word used to build the published model
/// string. Returns `None` on a bus error.
///
/// `IC_DEVICE_ID` is the only identity register the datasheet
/// documents, framed as a plain `PMBus` word read, matching every other
/// multi-byte register in this driver.
pub(crate) async fn read_device_id<I, E>(i2c: &mut I, address: u8) -> Option<u16>
where
    I: I2c<Error = E>,
    E: embedded_hal_async::i2c::Error,
{
    let mut word_buf = [0u8; 2];
    i2c.write_read(address, &[IC_DEVICE_ID], &mut word_buf)
        .await
        .ok()?;
    Some(u16::from_le_bytes(word_buf))
}
