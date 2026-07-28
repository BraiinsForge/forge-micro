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

//! Byte-level chip serial transport (sans-IO seam).
//!
//! [`Transport`] is the crate's I/O boundary: the raw UART byte stream, a
//! runtime baud switch, and a settle-delay timer. The consumer
//! (`ubos-mining`) implements it over the real serial link; every
//! vendor's wire framing lives above it inside this crate, so `ubos-asic`
//! stays `#![no_std]`, performs no direct I/O, and is unit-testable
//! against a mock transport.
//!
//! This is the lowered replacement for the register-shaped `ChipBus`:
//! different chip families frame incompatibly, so the shared seam sits
//! below framing, at the level of bytes and baud.

/// A failed host-side baud switch. Carries no backend detail: the
/// transport logs the specifics itself; the driver only needs to know
/// the line is no longer trustworthy so bring-up can fail and retry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BaudRateError;

/// The chip-side serial link a [`crate::chain::ChainDriver`] drives.
///
/// Register and command framing is the driver's concern; this trait only
/// moves bytes, switches the line rate, and waits. Reads are cancel-safe:
/// the implementation buffers incoming bytes so a dropped `read` future
/// loses no data.
// The async-fn-in-trait pattern matches the project-wide convention
// established by `ubos_psu::Psu` and the former `ChipBus`. Send bounds
// are not required on this single-task embedded target.
#[allow(async_fn_in_trait)]
pub trait Transport {
    /// Write every byte of `bytes` to the line.
    async fn write_all(&mut self, bytes: &[u8]);

    /// Read available bytes into `buf` within `timeout_ms`, returning how
    /// many were written. Returns `0` on timeout or end-of-stream. A
    /// driver bounds its wait for chip replies this way — enumeration and
    /// nonce-FIFO reads stop once the line goes quiet. Cancel-safe.
    async fn read(&mut self, buf: &mut [u8], timeout_ms: u32) -> usize;

    /// Switch the line to `hz` baud. Callers insert their own settle
    /// delay around the switch via [`Self::delay_ms`]; the switch itself
    /// is synchronous.
    ///
    /// # Errors
    ///
    /// A failed switch must return `Err` so bring-up aborts and retries
    /// from a clean reset: the chips have already moved to the fast rate
    /// by the time the host switches, so a silently missed switch would
    /// leave the miner "running" on a misclocked line that can never
    /// carry a share.
    fn set_baud(&mut self, hz: u32) -> Result<(), BaudRateError>;

    /// Wait `ms` milliseconds, for the chip-settle delays a bring-up
    /// recipe inserts between writes.
    async fn delay_ms(&mut self, ms: u32);
}
