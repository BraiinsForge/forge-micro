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

//! Logging macro facade that swaps between `defmt` and `log` at compile time.
//!
//! Consumer crates call `ubos_logging::{info, warn, error, debug, trace}`
//! unconditionally. With the `defmt` feature enabled the macros expand to
//! `defmt::*!` and inherit defmt's format specifiers; without it they expand
//! to `log::*!` and use `core::fmt`. The firmware builds turn `defmt` on;
//! the host binary leaves it off and installs an `env_logger`.
//!
//! Types that `derive(defmt::Format)` must gate the derive themselves, since
//! the derive lives on the type definition and cannot be forwarded through a
//! macro facade. Call sites that use defmt-specific specifiers such as
//! `{=[u8]:a}` or `defmt::Debug2Format` must also be rewritten to a form that
//! `core::fmt` understands (typically `{:?}` or pre-formatted `&str`).

#![no_std]

#[cfg(feature = "defmt")]
pub use defmt as __backend;

#[cfg(not(feature = "defmt"))]
pub use log as __backend;

/// Adapter that lets a `Debug`-only type appear in a format string under both
/// backends. Under `defmt` it re-exports the native `defmt::Debug2Format`;
/// without it, a local newtype forwards `core::fmt::Debug`. Call sites write
/// `{:?}` and pass `Debug2Format(&error)` uniformly.
#[cfg(feature = "defmt")]
pub use defmt::Debug2Format;

#[cfg(not(feature = "defmt"))]
pub struct Debug2Format<'a, T: ?Sized>(pub &'a T);

#[cfg(not(feature = "defmt"))]
impl<T: core::fmt::Debug + ?Sized> core::fmt::Debug for Debug2Format<'_, T> {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.0.fmt(formatter)
    }
}

#[macro_export]
macro_rules! info {
    ($($arg:tt)*) => { $crate::__backend::info!($($arg)*) };
}

#[macro_export]
macro_rules! warn {
    ($($arg:tt)*) => { $crate::__backend::warn!($($arg)*) };
}

#[macro_export]
macro_rules! error {
    ($($arg:tt)*) => { $crate::__backend::error!($($arg)*) };
}

#[macro_export]
macro_rules! debug {
    ($($arg:tt)*) => { $crate::__backend::debug!($($arg)*) };
}

#[macro_export]
macro_rules! trace {
    ($($arg:tt)*) => { $crate::__backend::trace!($($arg)*) };
}
