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

//! Driver for the TI TPS53647 multiphase `PMBus` VRM controller.
//!
//! Output voltage is commanded in the VR12.0 VID format (5 mV steps
//! from a 250 mV floor); telemetry reads back in `PMBus` linear11.
//! All configuration is integer (mV / °C). The chip address is
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

use crate::Error;

// Register addresses (PMBus command codes).
pub(crate) const OPERATION: u8 = 0x01;
pub(crate) const CLEAR_FAULTS: u8 = 0x03;
pub(crate) const VOUT_MODE: u8 = 0x20;
pub(crate) const VOUT_COMMAND: u8 = 0x21;
pub(crate) const VOUT_MAX: u8 = 0x24;
pub(crate) const OT_FAULT_LIMIT: u8 = 0x4F;
pub(crate) const OT_WARN_LIMIT: u8 = 0x51;
pub(crate) const VIN_OV_FAULT_LIMIT: u8 = 0x55;
pub(crate) const STATUS_BYTE: u8 = 0x78;
pub(crate) const READ_VOUT: u8 = 0x8B;
pub(crate) const READ_POUT: u8 = 0x96;
pub(crate) const READ_PIN: u8 = 0x97;

/// OPERATION register on-bit: sets the output enabled state.
const OPERATION_ON: u8 = 1 << 7;

/// Required `VOUT_MODE` value: mode `001` (VID) with the VR12.0 code
/// type. The `SLEW_MODE` pinstrap latches the table at power-up; any
/// other readback means the board strap disagrees with this driver's
/// VID encoding and no voltage may be commanded.
pub(crate) const VOUT_MODE_VID_VR120: u8 = 0x21;

/// VR12.0 VID table: code 01h is 250 mV, each step adds 5 mV, code
/// 00h is "off".
pub(crate) const VID_FLOOR_MILLIVOLTS: u32 = 250;
pub(crate) const VID_STEP_MILLIVOLTS: u32 = 5;
/// Highest voltage the 8-bit table expresses (code `FFh`).
pub(crate) const VID_CEILING_MILLIVOLTS: u32 = 1520;

/// Encode a voltage in millivolts to the nearest VR12.0 VID code.
/// Callers guarantee `VID_FLOOR_MILLIVOLTS ..= VID_CEILING_MILLIVOLTS`.
pub(crate) const fn encode_vid(millivolts: u32) -> u8 {
    let steps = (millivolts - VID_FLOOR_MILLIVOLTS + VID_STEP_MILLIVOLTS / 2) / VID_STEP_MILLIVOLTS;
    (steps + 1) as u8
}

/// Decode a VR12.0 VID code to millivolts. Code 0 is "off" and
/// decodes to 0 mV.
pub(crate) const fn decode_vid_mv(vid: u8) -> u32 {
    if vid == 0 {
        return 0;
    }
    VID_FLOOR_MILLIVOLTS + (vid as u32 - 1) * VID_STEP_MILLIVOLTS
}

/// Decode the `STATUS_BYTE` register into the shared [`crate::StatusFlags`].
///
/// `PMBus` `STATUS_BYTE` bit mapping: bit5 = `VOUT_OV`, bit4 = `IOUT_OC`,
/// bit3 = `VIN_UV`, bit2 = temperature fault.
pub(crate) fn decode_status(byte: u8) -> crate::StatusFlags {
    crate::StatusFlags {
        overvoltage: (byte & (1 << 5)) != 0,
        current_limit: (byte & (1 << 4)) != 0,
        undervoltage: (byte & (1 << 3)) != 0,
        thermal_shutdown: (byte & (1 << 2)) != 0,
    }
}

/// Configuration for the TPS53647 VRM controller.
///
/// No overcurrent fields: the F-IMAX pinstrap fixes IMAX in hardware
/// (the OC fault defaults to 125% of it) and the fault-response
/// registers are read-only on this part.
#[derive(Clone, Copy, Debug)]
pub struct Tps53647Config {
    /// `PMBus` 7-bit address, set by the ADDR-TRISE resistor divider.
    pub address: u8,
    /// Output voltage to program during init. The rail stays off
    /// until `enable`; the tuner then anchors at open-core voltage.
    pub initial_millivolts: u32,
    /// VIN overvoltage fault threshold. 1 V register resolution.
    pub vin_ov_fault_millivolts: u32,
    /// Overtemperature fault threshold.
    pub ot_fault_celsius: u32,
    /// Overtemperature warning threshold.
    pub ot_warn_celsius: u32,
    /// Minimum voltage accepted by [`set_voltage`].
    pub voltage_min_millivolts: u32,
    /// Maximum voltage accepted by [`set_voltage`]; also programmed
    /// into `VOUT_MAX` so the device enforces it.
    pub voltage_max_millivolts: u32,
}

impl Tps53647Config {
    /// Default TPS53647 configuration. The address is the Braiins Axe
    /// ADDR-TRISE strap candidate, pending hardware confirmation.
    pub const DEFAULT: Self = Self {
        address: 0x71,
        initial_millivolts: VID_FLOOR_MILLIVOLTS,
        vin_ov_fault_millivolts: 14_000,
        ot_fault_celsius: 115,
        ot_warn_celsius: 95,
        voltage_min_millivolts: VID_FLOOR_MILLIVOLTS,
        voltage_max_millivolts: VID_CEILING_MILLIVOLTS,
    };
}

/// Write a `PMBus` word register (register + 2 LE data bytes).
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

/// Read a `PMBus` word register (2 LE data bytes).
async fn read_word<I, E>(i2c: &mut I, address: u8, register: u8) -> Result<u16, Error<E>>
where
    I: I2c<Error = E>,
    E: embedded_hal_async::i2c::Error,
{
    let mut buffer = [0u8; 2];
    i2c.write_read(address, &[register], &mut buffer)
        .await
        .map_err(Error::Bus)?;
    Ok(u16::from_le_bytes(buffer))
}

/// Verify the VID mode and program limits and the initial voltage.
///
/// Pinstraps and NVM already configured phases, IMAX, and fault
/// responses at power-up; only the host-adjustable limits and the
/// parked initial voltage are written here. Does not touch
/// `OPERATION`: the output stays off until [`enable`].
///
/// # Errors
///
/// [`Error::Bus`] on any I2C failure; [`Error::OutOfRange`] when
/// `VOUT_MODE` reports a VID table other than VR12.0, which means the
/// `SLEW_MODE` strap and this driver disagree on voltage encoding.
pub async fn init<I, E>(i2c: &mut I, config: &Tps53647Config) -> Result<(), Error<E>>
where
    I: I2c<Error = E>,
    E: embedded_hal_async::i2c::Error,
{
    let address = config.address;
    let mut buffer = [0u8; 1];
    i2c.write_read(address, &[VOUT_MODE], &mut buffer)
        .await
        .map_err(Error::Bus)?;
    if buffer[0] != VOUT_MODE_VID_VR120 {
        ubos_logging::error!(
            "TPS53647 VOUT_MODE {:#04x} is not VID/VR12.0; refusing to program",
            buffer[0]
        );
        return Err(Error::OutOfRange);
    }

    write_word(
        i2c,
        address,
        VOUT_MAX,
        u16::from(encode_vid(config.voltage_max_millivolts)),
    )
    .await?;
    // OT limits are linear11 with exponent 0: plain degrees Celsius.
    write_word(i2c, address, OT_FAULT_LIMIT, config.ot_fault_celsius as u16).await?;
    write_word(i2c, address, OT_WARN_LIMIT, config.ot_warn_celsius as u16).await?;
    // VIN_OV is linear11 with exponent 0: whole volts, rounded.
    write_word(
        i2c,
        address,
        VIN_OV_FAULT_LIMIT,
        ((config.vin_ov_fault_millivolts + 500) / 1000) as u16,
    )
    .await?;
    write_word(
        i2c,
        address,
        VOUT_COMMAND,
        u16::from(encode_vid(config.initial_millivolts)),
    )
    .await?;

    i2c.write(address, &[CLEAR_FAULTS])
        .await
        .map_err(Error::Bus)
}

/// Enable the output by setting the OPERATION on-bit
/// (read-modify-write), then clear latched faults.
///
/// # Errors
///
/// Returns [`Error::Bus`] on any I2C transaction failure.
pub async fn enable<I, E>(i2c: &mut I, config: &Tps53647Config) -> Result<(), Error<E>>
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

/// Disable the output by clearing the OPERATION on-bit.
///
/// # Errors
///
/// Returns [`Error::Bus`] on any I2C transaction failure.
pub async fn disable<I, E>(i2c: &mut I, config: &Tps53647Config) -> Result<(), Error<E>>
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

/// Command an output voltage in millivolts.
///
/// # Errors
///
/// [`Error::OutOfRange`] outside the configured window; [`Error::Bus`]
/// on I2C failure.
pub async fn set_voltage<I, E>(
    i2c: &mut I,
    config: &Tps53647Config,
    millivolts: u32,
) -> Result<(), Error<E>>
where
    I: I2c<Error = E>,
    E: embedded_hal_async::i2c::Error,
{
    if millivolts < config.voltage_min_millivolts || millivolts > config.voltage_max_millivolts {
        return Err(Error::OutOfRange);
    }
    write_word(
        i2c,
        config.address,
        VOUT_COMMAND,
        u16::from(encode_vid(millivolts)),
    )
    .await
}

/// Read the measured output voltage in millivolts.
///
/// `READ_VOUT` returns a VID code, not linear11; the linear-format
/// alternative (`MFR_SPECIFIC_04`) is not used so all voltage paths
/// share one codec.
///
/// # Errors
///
/// Returns [`Error::Bus`] on any I2C transaction failure.
pub async fn read_vout_mv<I, E>(i2c: &mut I, config: &Tps53647Config) -> Result<u32, Error<E>>
where
    I: I2c<Error = E>,
    E: embedded_hal_async::i2c::Error,
{
    let raw = read_word(i2c, config.address, READ_VOUT).await?;
    Ok(decode_vid_mv(raw as u8))
}

/// Read the output power in milliwatts.
///
/// # Errors
///
/// Returns [`Error::Bus`] on any I2C transaction failure.
pub async fn read_pout_mw<I, E>(i2c: &mut I, config: &Tps53647Config) -> Result<u32, Error<E>>
where
    I: I2c<Error = E>,
    E: embedded_hal_async::i2c::Error,
{
    let raw = read_word(i2c, config.address, READ_POUT).await?;
    Ok(unsigned_milli(crate::sic45x::decode_ulin11_milli(raw)))
}

/// Read the input power in milliwatts.
///
/// # Errors
///
/// Returns [`Error::Bus`] on any I2C transaction failure.
pub async fn read_pin_mw<I, E>(i2c: &mut I, config: &Tps53647Config) -> Result<u32, Error<E>>
where
    I: I2c<Error = E>,
    E: embedded_hal_async::i2c::Error,
{
    let raw = read_word(i2c, config.address, READ_PIN).await?;
    Ok(unsigned_milli(crate::sic45x::decode_ulin11_milli(raw)))
}

/// Clamp a decoded telemetry value to zero; power readings cannot be
/// negative, so a negative decode is residual noise around zero load.
const fn unsigned_milli(value: i32) -> u32 {
    if value < 0 {
        return 0;
    }
    #[allow(clippy::cast_sign_loss)]
    {
        value as u32
    }
}

/// Read `STATUS_BYTE` and decode the four shared fault flags.
///
/// # Errors
///
/// Returns [`Error::Bus`] on any I2C transaction failure.
pub async fn read_status<I, E>(
    i2c: &mut I,
    config: &Tps53647Config,
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
#[path = "tests/tps53647.rs"]
mod tests;
