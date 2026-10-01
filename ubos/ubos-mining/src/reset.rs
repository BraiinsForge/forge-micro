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

//! Hardware reset for the BM1362 chip chain.
//!
//! Active-low on the BMM101: backends drive the pin LOW to assert
//! reset and HIGH to release it. Methods are infallible: a failed
//! toggle is unrecoverable from the caller, so it is logged internally
//! and left for the next chip operation to surface.
//!
//! The methods are `async` because some backends drive the line
//! through an I2C I/O expander — the esp32s3 carrier's TCA9554 on
//! EXIO5 — which needs an async bus transaction. GPIO-backed boards
//! implement them as plain non-awaiting bodies.

use embedded_hal::digital::OutputPin;

// async fn in a public trait: the only implementors are the per-board
// reset pins, consumed by `Miner` through a generic bound, never across
// an auto-trait boundary, so the Send-leakage caveat does not apply.
#[allow(async_fn_in_trait)]
pub trait ResetPin {
    /// Hold the chip in reset (drive the line LOW).
    async fn enter_reset(&mut self);

    /// Release the chip from reset (drive the line HIGH).
    async fn exit_reset(&mut self);

    /// Called once the chain is enumerated, configured and clocked, and
    /// immediately before the first work is dispatched.
    ///
    /// The default does nothing; a chain whose mode line is held in one
    /// state across enumeration and configuration and must move to
    /// another to hash overrides it, since enumerating that chain
    /// already in the hashing state answers only after several
    /// attempts, if at all.
    ///
    /// [`Self::exit_reset`] is the other half of that pair: a board with
    /// a mode line leaves it in the configuring state there and moves it
    /// here.
    async fn enter_work_mode(&mut self) {}
}

/// A reset line driven straight from one GPIO output.
///
/// The `ResetPin` trait carries no timings, so a board whose reset line is
/// a plain push-pull pad needs nothing beyond two level writes. Every such
/// board wraps its HAL output in this type instead of writing the same
/// impl again. A board whose line needs settle times or a mode line writes
/// its own impl.
///
/// The pin writes are infallible on every HAL this crate targets, so a
/// reported error is dropped, as the trait doc says.
pub struct OutputResetPin<P: OutputPin> {
    pin: P,
}

impl<P: OutputPin> OutputResetPin<P> {
    #[must_use]
    pub fn new(pin: P) -> Self {
        Self { pin }
    }
}

impl<P: OutputPin> ResetPin for OutputResetPin<P> {
    async fn enter_reset(&mut self) {
        let _ = self.pin.set_low();
    }

    async fn exit_reset(&mut self) {
        let _ = self.pin.set_high();
    }
}
