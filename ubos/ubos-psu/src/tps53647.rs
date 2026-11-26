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
//! All configuration is in typed units. The chip address is
//! supplied by the caller — this module is address-parameterized.
//!
//! Every operation here takes the bus as `&mut I` and the device config
//! by reference rather than a method on a handle, for the same reason
//! as [`crate::sic45x`]: see its module doc.

use embedded_hal_async::i2c::{I2c, SevenBitAddress};
use ubos_core::units::{Power, Temperature, Voltage};

use crate::Error;
use crate::pmbus::{decode_status, read_word, write_word};
use crate::sic45x::RawUlinear11;

#[cfg(test)]
#[path = "tests/tps53647.rs"]
mod tests;

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
pub(crate) const MFR_SPECIFIC_20: u8 = 0xE4;

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

/// Encode a voltage to the nearest VR12.0 VID code.
/// Callers guarantee `VID_FLOOR_MILLIVOLTS ..= VID_CEILING_MILLIVOLTS`.
pub(crate) const fn encode_vid(voltage: Voltage) -> u8 {
    let steps =
        (voltage.mv() - VID_FLOOR_MILLIVOLTS + VID_STEP_MILLIVOLTS / 2) / VID_STEP_MILLIVOLTS;
    (steps + 1) as u8
}

/// A raw VR12.0 VID code, as `READ_VOUT` returns it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RawVid(pub(crate) u8);

/// Decode a VR12.0 VID code to a voltage. Code 0 is "off" and
/// decodes to 0 mV.
impl From<RawVid> for Voltage {
    fn from(raw: RawVid) -> Self {
        if raw.0 == 0 {
            return Self::from_mv(0);
        }
        Self::from_mv(VID_FLOOR_MILLIVOLTS + (u32::from(raw.0) - 1) * VID_STEP_MILLIVOLTS)
    }
}

/// Encode a VIN limit to the register format: linear11 with exponent 0,
/// whole volts, rounded.
pub(crate) const fn encode_vin_ov(voltage: Voltage) -> u16 {
    ((voltage.mv() + 500) / 1000) as u16
}

/// Maximum operational phase count, the `PHASE_NUM` field of
/// `MFR_SPECIFIC_20`. The part runs one to four
/// phases, so the register's five- and six-phase codes are not modelled.
/// The CSP pinstraps bound it: a count above what the board populates is
/// ignored by the device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phases {
    One,
    Two,
    Three,
    Four,
}

impl Phases {
    /// The `PHASE_NUM` code; the reserved bits 7:3 stay zero.
    pub(crate) const fn phase_num(self) -> u8 {
        match self {
            Self::One => 0b000,
            Self::Two => 0b001,
            Self::Three => 0b010,
            Self::Four => 0b011,
        }
    }
}

/// Configuration for the TPS53647 VRM controller.
///
/// No overcurrent fields: the F-IMAX pinstrap fixes IMAX in hardware,
/// defaulting the OC fault to 125% of it, and the fault-response
/// registers are read-only on this part.
#[derive(Clone, Copy, Debug)]
pub struct Tps53647Config {
    /// `PMBus` 7-bit address, set by the ADDR-TRISE resistor divider.
    pub address: SevenBitAddress,
    /// Output voltage to program during init. The rail stays off
    /// until `enable`; the tuner then anchors at open-core voltage.
    pub initial_voltage: Voltage,
    /// VIN overvoltage fault threshold. 1 V register resolution.
    pub vin_ov_fault: Voltage,
    /// Overtemperature fault threshold.
    pub ot_fault: Temperature,
    /// Overtemperature warning threshold.
    pub ot_warn: Temperature,
    /// Minimum voltage accepted by [`set_voltage`].
    pub voltage_min: Voltage,
    /// Maximum voltage accepted by [`set_voltage`]; also programmed
    /// into `VOUT_MAX` so the device enforces it.
    pub voltage_max: Voltage,
    /// Phase count to program during init; `None` keeps the count the
    /// CSP pinstraps latched at power-up.
    pub phases: Option<Phases>,
}

impl Tps53647Config {
    /// Default TPS53647 configuration. The address is the Forge Axe
    /// ADDR-TRISE strap candidate, pending hardware confirmation.
    pub const DEFAULT: Self = Self {
        address: 0x71,
        initial_voltage: Voltage::from_mv(VID_FLOOR_MILLIVOLTS),
        vin_ov_fault: Voltage::from_mv(14_000),
        ot_fault: Temperature::from_celsius(115),
        ot_warn: Temperature::from_celsius(95),
        voltage_min: Voltage::from_mv(VID_FLOOR_MILLIVOLTS),
        voltage_max: Voltage::from_mv(VID_CEILING_MILLIVOLTS),
        phases: None,
    };
}

/// Verify the VID mode and program the phase count, limits and the
/// initial voltage.
///
/// Pinstraps latch the maximum phase count, and pinstraps and NVM set
/// IMAX and the fault responses at power-up; only the phase count, the
/// host-adjustable limits and the parked initial voltage are written
/// here. Does not touch `OPERATION`: the output stays off until
/// [`enable`].
///
/// # Errors
///
/// [`Error::Bus`] on any I2C failure; [`Error::OutOfRange`] when
/// `VOUT_MODE` reports a VID table other than VR12.0, which means the
/// `SLEW_MODE` strap and this driver disagree on voltage encoding, or
/// when the phase count does not read back, which means the pinstraps
/// populate fewer phases than configured.
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

    if let Some(phases) = config.phases {
        let phase_num = phases.phase_num();
        i2c.write(address, &[MFR_SPECIFIC_20, phase_num])
            .await
            .map_err(Error::Bus)?;
        // The device keeps its previous count, rather than failing the
        // write, when the pinstraps populate fewer phases than requested.
        i2c.write_read(address, &[MFR_SPECIFIC_20], &mut buffer)
            .await
            .map_err(Error::Bus)?;
        if buffer[0] != phase_num {
            ubos_logging::error!(
                "TPS53647 PHASE_NUM {:#04x} did not read back as {:#04x}; refusing to program",
                buffer[0],
                phase_num
            );
            return Err(Error::OutOfRange);
        }
    }

    write_word(
        i2c,
        address,
        VOUT_MAX,
        u16::from(encode_vid(config.voltage_max)),
    )
    .await?;
    // OT limits are linear11 with exponent 0: plain degrees Celsius.
    write_word(
        i2c,
        address,
        OT_FAULT_LIMIT,
        crate::sic45x::encode_temp(config.ot_fault),
    )
    .await?;
    write_word(
        i2c,
        address,
        OT_WARN_LIMIT,
        crate::sic45x::encode_temp(config.ot_warn),
    )
    .await?;
    // VIN_OV is linear11 with exponent 0: whole volts, rounded.
    write_word(
        i2c,
        address,
        VIN_OV_FAULT_LIMIT,
        encode_vin_ov(config.vin_ov_fault),
    )
    .await?;
    write_word(
        i2c,
        address,
        VOUT_COMMAND,
        u16::from(encode_vid(config.initial_voltage)),
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

/// Command an output voltage and return the voltage of the VID code
/// written, which is the nearest step to `voltage`.
///
/// # Errors
///
/// [`Error::OutOfRange`] outside the configured window; [`Error::Bus`]
/// on I2C failure.
pub async fn set_voltage<I, E>(
    i2c: &mut I,
    config: &Tps53647Config,
    voltage: Voltage,
) -> Result<Voltage, Error<E>>
where
    I: I2c<Error = E>,
    E: embedded_hal_async::i2c::Error,
{
    if voltage < config.voltage_min || voltage > config.voltage_max {
        return Err(Error::OutOfRange);
    }
    let vid = encode_vid(voltage);
    write_word(i2c, config.address, VOUT_COMMAND, u16::from(vid)).await?;
    Ok(RawVid(vid).into())
}

/// Read the measured output voltage.
///
/// `READ_VOUT` returns a VID code, not linear11, unlike the unused
/// `MFR_SPECIFIC_04` alternative; every voltage path here shares this
/// one codec.
///
/// # Errors
///
/// Returns [`Error::Bus`] on any I2C transaction failure.
pub async fn read_vout<I, E>(i2c: &mut I, config: &Tps53647Config) -> Result<Voltage, Error<E>>
where
    I: I2c<Error = E>,
    E: embedded_hal_async::i2c::Error,
{
    let raw = read_word(i2c, config.address, READ_VOUT).await?;
    Ok(RawVid(raw as u8).into())
}

/// Read the output power.
///
/// # Errors
///
/// Returns [`Error::Bus`] on any I2C transaction failure.
pub async fn read_pout<I, E>(i2c: &mut I, config: &Tps53647Config) -> Result<Power, Error<E>>
where
    I: I2c<Error = E>,
    E: embedded_hal_async::i2c::Error,
{
    let raw = read_word(i2c, config.address, READ_POUT).await?;
    Ok(RawUlinear11(raw).into())
}

/// Read the input power.
///
/// # Errors
///
/// Returns [`Error::Bus`] on any I2C transaction failure.
pub async fn read_pin<I, E>(i2c: &mut I, config: &Tps53647Config) -> Result<Power, Error<E>>
where
    I: I2c<Error = E>,
    E: embedded_hal_async::i2c::Error,
{
    let raw = read_word(i2c, config.address, READ_PIN).await?;
    Ok(RawUlinear11(raw).into())
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
