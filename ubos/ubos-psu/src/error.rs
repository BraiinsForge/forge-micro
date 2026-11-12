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

//! The errors a supply's driver and its handle answer with.

use embedded_hal_async::i2c::ErrorKind;

use crate::StatusFlags;

#[cfg(test)]
#[path = "tests/error.rs"]
mod tests;

/// Errors surfaced by the [`crate::Psu`] trait. `E` is the underlying I2C
/// bus error type.
#[derive(Debug)]
pub enum Error<E> {
    Bus(E),
    /// Init failed; only the standardized kind is bubbled up
    /// because there is no `Clone` bound on `E`. The concrete
    /// error is logged at the call site that first observed it.
    InitFailed(embedded_hal_async::i2c::ErrorKind),
    OutOfRange,
    Fault(StatusFlags),
    Terminated,
    /// A bounded retry or poll loop exhausted its budget without ever
    /// observing the expected device state. Distinct from `Bus`: every
    /// individual transaction in the loop completed without a bus
    /// error, but the loop's expected condition never became true.
    Timeout,
}

/// The error a [`crate::Handle`] command answers with: the driver's error with
/// its bus error reduced to the standard kind, so no consumer is generic
/// over the board's bus.
pub type CommandError = Error<ErrorKind>;

impl<E: embedded_hal_async::i2c::Error> Error<E> {
    /// Reduce a driver error to the standard bus error kind.
    pub(crate) fn reduce(self) -> CommandError {
        match self {
            Error::Bus(bus) => Error::Bus(bus.kind()),
            Error::InitFailed(kind) => Error::InitFailed(kind),
            Error::OutOfRange => Error::OutOfRange,
            Error::Fault(flags) => Error::Fault(flags),
            Error::Terminated => Error::Terminated,
            Error::Timeout => Error::Timeout,
        }
    }
}
