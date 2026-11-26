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

//! Shared `PMBus` primitives.
//!
//! The register access and status decoding every `PMBus` part on a
//! board answers the same way.
//!
//! `tps6287x` is not a `PMBus` part and decodes its own status layout,
//! so it does not use [`decode_status`] and must not be made to.
//!
//! These are free functions taking the bus as `&mut I` for the same
//! reason as [`crate::sic45x`]: see its module doc.

use embedded_hal_async::i2c::{I2c, SevenBitAddress};

use crate::Error;

/// Write a `PMBus` word register: the register byte, then two
/// little-endian data bytes.
///
/// # Errors
///
/// Returns [`Error::Bus`] on any I2C transaction failure.
pub(crate) async fn write_word<I, E>(
    i2c: &mut I,
    address: SevenBitAddress,
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

/// Read a `PMBus` word register: two little-endian data bytes.
///
/// # Errors
///
/// Returns [`Error::Bus`] on any I2C transaction failure.
pub(crate) async fn read_word<I, E>(
    i2c: &mut I,
    address: SevenBitAddress,
    register: u8,
) -> Result<u16, Error<E>>
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

/// Decode the standard `PMBus` `STATUS_BYTE` into the shared
/// [`crate::StatusFlags`].
///
/// The layout is the specification's, not any one part's: bit 5 is
/// `VOUT_OV`, bit 4 `IOUT_OC`, bit 3 `VIN_UV` and bit 2 a temperature
/// fault.
pub(crate) fn decode_status(byte: u8) -> crate::StatusFlags {
    crate::StatusFlags {
        overvoltage: (byte & (1 << 5)) != 0,
        current_limit: (byte & (1 << 4)) != 0,
        undervoltage: (byte & (1 << 3)) != 0,
        thermal_shutdown: (byte & (1 << 2)) != 0,
    }
}
