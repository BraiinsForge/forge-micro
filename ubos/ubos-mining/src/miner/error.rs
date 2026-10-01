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

//! The errors of the chip-control operations of the miner.

use embedded_io::ErrorKind;
use ubos_core::units::Frequency;
use ubos_psu::Error as PsuError;

/// Error type for chip-control operations. `Serial` wraps the
/// `embedded_io` error kind, so a caller is not coupled to the serial
/// actor's types.
#[derive(Debug)]
pub enum MinerError {
    /// The serial actor returned an error on a control operation.
    Serial(ErrorKind),
    /// The PSU rejected a voltage or enable/disable command. The
    /// payload narrows the failure to a discriminant the caller can
    /// dispatch on without taking on the PSU's `BusError` generic.
    Psu(PsuErrorKind),
    /// The chain driver rejected a frequency outside the chip's range.
    UnsupportedFrequency(Frequency),
}

impl From<ubos_asic::ChipError> for MinerError {
    fn from(error: ubos_asic::ChipError) -> Self {
        match error {
            ubos_asic::ChipError::UnsupportedFrequency(freq) => {
                MinerError::UnsupportedFrequency(freq)
            }
            // A refused host baud switch is a serial-line failure; the
            // rate itself is already logged by the transport.
            ubos_asic::ChipError::BaudSwitch(_) => MinerError::Serial(ErrorKind::Other),
        }
    }
}

impl From<embedded_io::ErrorKind> for MinerError {
    fn from(kind: embedded_io::ErrorKind) -> Self {
        MinerError::Serial(kind)
    }
}

/// Classification of [`ubos_psu::Error`] surfaces stripped of the
/// generic bus-error payload so [`MinerError`] does not have to be
/// parameterized over the PSU's `BusError` type.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PsuErrorKind {
    Bus,
    InitFailed,
    OutOfRange,
    Fault,
    Terminated,
    Timeout,
}

impl<E> From<PsuError<E>> for PsuErrorKind {
    fn from(error: PsuError<E>) -> Self {
        match error {
            PsuError::Bus(_) => PsuErrorKind::Bus,
            PsuError::InitFailed(_) => PsuErrorKind::InitFailed,
            PsuError::OutOfRange => PsuErrorKind::OutOfRange,
            PsuError::Fault(_) => PsuErrorKind::Fault,
            PsuError::Terminated => PsuErrorKind::Terminated,
            PsuError::Timeout => PsuErrorKind::Timeout,
        }
    }
}

#[cfg(feature = "defmt")]
impl defmt::Format for MinerError {
    fn format(&self, f: defmt::Formatter) {
        match self {
            MinerError::Serial(kind) => {
                defmt::write!(f, "Serial({:?})", defmt::Debug2Format(kind));
            }
            MinerError::Psu(kind) => {
                defmt::write!(f, "Psu({:?})", defmt::Debug2Format(kind));
            }
            MinerError::UnsupportedFrequency(freq) => {
                defmt::write!(f, "UnsupportedFrequency({} kHz)", freq.khz());
            }
        }
    }
}
