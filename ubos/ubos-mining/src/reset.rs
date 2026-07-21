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
//! reset and HIGH to release it. Methods are infallible — a failed
//! toggle is unrecoverable from the caller's perspective, so the
//! convention is "log internally and proceed"; the next chip
//! operation will surface the underlying issue.
//!
//! The methods are `async` because some backends drive the line
//! through an I2C I/O expander (the esp32s3 carrier's TCA9554 on
//! EXIO5), which needs an async bus transaction. GPIO-backed boards
//! implement them as plain non-awaiting bodies.

// async fn in a public trait: the only implementors are the per-board
// reset pins, consumed by `Miner` through a generic bound, never across
// an auto-trait boundary, so the Send-leakage caveat does not apply.
#[allow(async_fn_in_trait)]
pub trait ResetPin {
    /// Hold the chip in reset (drive the line LOW).
    async fn enter_reset(&mut self);

    /// Release the chip from reset (drive the line HIGH).
    async fn exit_reset(&mut self);
}
