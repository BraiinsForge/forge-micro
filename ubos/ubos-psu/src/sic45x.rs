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
//! All configuration is integer (mV / mA / °C). The chip address is
//! supplied by the caller — this module is address-parameterized.

use embedded_hal_async::i2c::I2c;

use crate::Error;

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
pub(crate) const POWER_GOOD_ON: u8 = 0x5E;
pub(crate) const POWER_GOOD_OFF: u8 = 0x5F;
pub(crate) const STATUS_BYTE: u8 = 0x78;
pub(crate) const READ_VOUT: u8 = 0x8B;
pub(crate) const READ_POUT: u8 = 0x96;

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

/// Encode a VOUT value in millivolts to the ULINEAR16 register format.
///
/// LSB = 1/512 V. Rounds to nearest representable value.
pub(crate) const fn encode_vout(millivolts: u32) -> u16 {
    ((millivolts * 512 + 500) / 1000) as u16
}

/// Decode a ULINEAR16 VOUT register reading to millivolts.
///
/// Inverse of [`encode_vout`]: LSB = 1/512 V, rounded to nearest mV.
pub(crate) const fn decode_vout_mv(raw: u16) -> u32 {
    (raw as u32 * 1000 + 256) / 512
}

/// Encode a millivolt or milliamp value to LINEAR11 format with
/// exponent −1, so the LSB is 0.5 units.
///
/// Used for VIN limits in millivolts and IOUT limits in milliamps.
pub(crate) const fn encode_lin11_half(milli: u32) -> u16 {
    // Exponent field -1 = 0x1F in 5-bit two's complement → upper 5
    // bits = 11111 = 0xF8xx. Mantissa = milli / 0.5 = milli*2/1000,
    // rounded to nearest: (milli + 250) / 500.
    0xF800 | (((milli + 250) / 500) as u16)
}

/// Encode a temperature in whole degrees Celsius to LINEAR11 format
/// with exponent 0 (LSB = 1 °C).
pub(crate) const fn encode_temp(celsius: u32) -> u16 {
    // Temperature limits are whole degrees well under u16::MAX, so the
    // truncation never loses significant bits.
    celsius as u16
}

/// Decode a `ULINEAR11` register value (`READ_POUT` / `READ_PIN`) to
/// milli-units.
///
/// `READ_POUT` and `READ_PIN` use an unsigned 11-bit mantissa with a signed
/// 5-bit two's-complement exponent (per the `SIC45x` / `PMBus` spec), unlike
/// the signed-mantissa `LINEAR11` used for VIN/IOUT. The result is always
/// non-negative.
pub(crate) const fn decode_ulin11_milli(raw: u16) -> i32 {
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

/// Decode the `STATUS_BYTE` register into the shared [`crate::StatusFlags`] type.
///
/// `SIC45x` `STATUS_BYTE` bit mapping: bit5 = `VOUT_OV`, bit4 = `IOUT_OC`,
/// bit3 = `VIN_UV`, bit2 = temperature fault.
pub(crate) fn decode_status(byte: u8) -> crate::StatusFlags {
    crate::StatusFlags {
        overvoltage: (byte & (1 << 5)) != 0,
        current_limit: (byte & (1 << 4)) != 0,
        undervoltage: (byte & (1 << 3)) != 0,
        thermal_shutdown: (byte & (1 << 2)) != 0,
    }
}

/// Configuration for the `SIC45x` `PMBus` DC/DC converter.
///
/// All fields use integer units (mV, mA, °C). Embed this in a board
/// config struct; pass a reference to every driver function.
#[derive(Clone, Copy, Debug)]
pub struct Sic45xConfig {
    /// `PMBus` 7-bit address, selected by hardware resistor.
    pub address: u8,
    /// Output voltage to program during init.
    pub initial_millivolts: u32,
    /// Feedback divider scale to match the board's resistor network.
    pub loop_scale: LoopScale,
    /// VOUT overvoltage fault threshold.
    pub vout_ov_fault_millivolts: u32,
    /// VOUT undervoltage fault threshold.
    pub vout_uv_fault_millivolts: u32,
    /// VIN overvoltage fault threshold.
    pub vin_ov_fault_millivolts: u32,
    /// VIN undervoltage warning threshold.
    pub vin_uv_warn_millivolts: u32,
    /// IOUT overcurrent fault threshold.
    pub iout_oc_fault_milliamps: u32,
    /// IOUT overcurrent warning threshold.
    pub iout_oc_warn_milliamps: u32,
    /// Overtemperature fault threshold.
    pub ot_fault_celsius: u32,
    /// Overtemperature warning threshold.
    pub ot_warn_celsius: u32,
    /// `POWER_GOOD` asserts when VOUT rises above this threshold.
    pub power_good_on_millivolts: u32,
    /// `POWER_GOOD` deasserts when VOUT falls below this threshold.
    pub power_good_off_millivolts: u32,
    /// Minimum voltage accepted by [`set_voltage`].
    pub voltage_min_millivolts: u32,
    /// Maximum voltage accepted by [`set_voltage`].
    pub voltage_max_millivolts: u32,
}

impl Sic45xConfig {
    /// Default `SIC45x` configuration. The operating-point values are
    /// taken from the Femto miner.
    pub const DEFAULT: Self = Self {
        address: 0x10,
        initial_millivolts: 600,
        loop_scale: LoopScale::Scale0_5,
        vout_ov_fault_millivolts: 2650,
        vout_uv_fault_millivolts: 350,
        vin_ov_fault_millivolts: 18_000,
        vin_uv_warn_millivolts: 9_000,
        iout_oc_fault_milliamps: 25_000,
        iout_oc_warn_milliamps: 20_000,
        ot_fault_celsius: 115,
        ot_warn_celsius: 95,
        power_good_on_millivolts: 540,
        power_good_off_millivolts: 510,
        voltage_min_millivolts: 400,
        voltage_max_millivolts: 2400,
    };
}

/// Write a `PMBus` word register (register + 2 LE data bytes) to the bus.
async fn write_word<I, E>(
    i2c: &mut I,
    address: u8,
    register: u8,
    value: u16,
) -> Result<(), Error<E>>
where
    I: I2c<Error = E>,
    E: embedded_hal_async::i2c::Error,
{
    let [low_byte, high_byte] = value.to_le_bytes();
    i2c.write(address, &[register, low_byte, high_byte])
        .await
        .map_err(Error::Bus)
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
        encode_vout(config.vout_ov_fault_millivolts),
    )
    .await?;
    write_word(
        i2c,
        address,
        VOUT_UV_FAULT_LIMIT,
        encode_vout(config.vout_uv_fault_millivolts),
    )
    .await?;
    write_word(
        i2c,
        address,
        VIN_OV_FAULT_LIMIT,
        encode_lin11_half(config.vin_ov_fault_millivolts),
    )
    .await?;
    write_word(
        i2c,
        address,
        IOUT_OC_FAULT_LIMIT,
        encode_lin11_half(config.iout_oc_fault_milliamps),
    )
    .await?;
    write_word(
        i2c,
        address,
        OT_FAULT_LIMIT,
        encode_temp(config.ot_fault_celsius),
    )
    .await?;
    write_word(
        i2c,
        address,
        VIN_UV_WARN_LIMIT,
        encode_lin11_half(config.vin_uv_warn_millivolts),
    )
    .await?;
    write_word(
        i2c,
        address,
        IOUT_OC_WARN_LIMIT,
        encode_lin11_half(config.iout_oc_warn_milliamps),
    )
    .await?;
    write_word(
        i2c,
        address,
        OT_WARN_LIMIT,
        encode_temp(config.ot_warn_celsius),
    )
    .await?;

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
        encode_vout(config.power_good_on_millivolts),
    )
    .await?;
    write_word(
        i2c,
        address,
        POWER_GOOD_OFF,
        encode_vout(config.power_good_off_millivolts),
    )
    .await?;
    write_word(
        i2c,
        address,
        VOUT_COMMAND,
        encode_vout(config.initial_millivolts),
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
/// `millivolts` is outside the configured limits. Returns
/// [`Error::Bus`] on I2C failure.
pub async fn set_voltage<I, E>(
    i2c: &mut I,
    config: &Sic45xConfig,
    millivolts: u32,
) -> Result<(), Error<E>>
where
    I: I2c<Error = E>,
    E: embedded_hal_async::i2c::Error,
{
    if millivolts < config.voltage_min_millivolts || millivolts > config.voltage_max_millivolts {
        return Err(Error::OutOfRange);
    }
    let bytes = encode_vout(millivolts).to_le_bytes();
    i2c.write(config.address, &[VOUT_COMMAND, bytes[0], bytes[1]])
        .await
        .map_err(Error::Bus)
}

/// Read the output power via `READ_POUT` and decode to milliwatts.
///
/// Negative decoded values (e.g. during initial ramp) are clamped
/// to zero.
///
/// # Errors
///
/// Returns [`Error::Bus`] on I2C failure.
pub async fn read_pout_mw<I, E>(i2c: &mut I, config: &Sic45xConfig) -> Result<u32, Error<E>>
where
    I: I2c<Error = E>,
    E: embedded_hal_async::i2c::Error,
{
    let mut buffer = [0u8; 2];
    i2c.write_read(config.address, &[READ_POUT], &mut buffer)
        .await
        .map_err(Error::Bus)?;
    let raw = u16::from_le_bytes(buffer);
    let milli = decode_ulin11_milli(raw);
    #[cfg(feature = "defmt")]
    ubos_logging::trace!(
        "SIC READ_POUT @{:x}: raw=[{:x} {:x}] word={:x} -> {} mW",
        config.address,
        buffer[0],
        buffer[1],
        raw,
        milli,
    );
    // milli.max(0) is always non-negative; the cast is safe.
    Ok(milli.max(0).cast_unsigned())
}

/// Read `READ_VOUT` and decode the actual output voltage to millivolts.
///
/// Unlike the INA230 (powered from the output rail, asleep until it
/// comes up), the SIC reads its own VOUT from VIN and is available as
/// soon as input power is present.
///
/// # Errors
///
/// Returns [`Error::Bus`] on I2C failure.
pub async fn read_vout_mv<I, E>(i2c: &mut I, config: &Sic45xConfig) -> Result<u32, Error<E>>
where
    I: I2c<Error = E>,
    E: embedded_hal_async::i2c::Error,
{
    let mut buffer = [0u8; 2];
    i2c.write_read(config.address, &[READ_VOUT], &mut buffer)
        .await
        .map_err(Error::Bus)?;
    let raw = u16::from_le_bytes(buffer);
    let millivolts = decode_vout_mv(raw);
    #[cfg(feature = "defmt")]
    ubos_logging::trace!(
        "SIC READ_VOUT @{:x}: raw=[{:x} {:x}] word={:x} -> {} mV",
        config.address,
        buffer[0],
        buffer[1],
        raw,
        millivolts,
    );
    Ok(millivolts)
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

#[cfg(test)]
#[path = "tests/sic45x.rs"]
mod tests;
