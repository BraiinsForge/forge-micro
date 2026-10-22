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

//! `SessionExtension` — a protocol that rides a mining session beside
//! the mining traffic, driven by the session loop.
//!
//! The loop owns the adapter and its one outbound slot, so an extension
//! never sends on its own: it keeps the frame it wants sent, the loop
//! offers that frame to the adapter whenever the slot is free, and the
//! loop calls back when the extension's own deadline passes. The trait
//! is object safe, so every session runs one loop body whatever it
//! carries.

use embassy_time::{Duration, Instant};
use mining_protocol::ExtensionFrame;

/// One protocol riding a mining session beside the mining traffic.
pub trait SessionExtension {
    /// A session has connected. Whatever the extension held from an
    /// earlier session is gone.
    fn connected(&mut self);

    /// A frame of a foreign extension arrived.
    fn frame(&mut self, frame: ExtensionFrame<'_>);

    /// When the loop should call `tick`, if ever.
    fn deadline(&self) -> Option<Instant>;

    /// The deadline passed while nothing waited in `outbound`.
    fn tick(&mut self);

    /// The frame waiting to be sent, if any.
    fn outbound(&self) -> Option<ExtensionFrame<'_>>;

    /// The frame `outbound` returned has been taken by the adapter.
    fn sent(&mut self);
}

/// The extension of a session that carries none.
pub struct NoExtension;

impl SessionExtension for NoExtension {
    fn connected(&mut self) {}

    fn frame(&mut self, _frame: ExtensionFrame<'_>) {}

    fn deadline(&self) -> Option<Instant> {
        None
    }

    fn tick(&mut self) {}

    fn outbound(&self) -> Option<ExtensionFrame<'_>> {
        None
    }

    fn sent(&mut self) {}
}

/// How long the loop waits before offering a waiting frame to the
/// adapter again. The slot frees on the next `drive`, which the next
/// inbound frame or share also triggers, so this only bounds the wait
/// on a quiet session.
pub const OUTBOUND_RETRY: Duration = Duration::from_millis(200);
