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

use embassy_time::Instant;
use mining_protocol::{Error, ExtensionFrame};

/// One protocol riding a mining session beside the mining traffic.
pub trait SessionExtension {
    /// A session has connected, authorised as `user`. Whatever the
    /// extension held from an earlier session is gone.
    fn connected(&mut self, user: &str);

    /// A frame of a foreign extension arrived.
    fn frame(&mut self, frame: ExtensionFrame<'_>);

    /// When the loop should call `tick`, if ever.
    fn deadline(&self) -> Option<Instant>;

    /// The deadline passed while nothing waited in `outbound`.
    fn tick(&mut self);

    /// The frame waiting to be sent, if any.
    fn outbound(&self) -> Option<ExtensionFrame<'_>>;

    /// The adapter accepted the frame into its queue, or rejected it
    /// permanently. Release the frame in either case. A busy slot
    /// keeps the frame pending and produces no callback.
    fn outbound_result(&mut self, result: Result<(), Error>);
}

/// The extension of a session that carries none.
pub struct NoExtension;

impl SessionExtension for NoExtension {
    fn connected(&mut self, _user: &str) {}

    fn frame(&mut self, _frame: ExtensionFrame<'_>) {}

    fn deadline(&self) -> Option<Instant> {
        None
    }

    fn tick(&mut self) {}

    fn outbound(&self) -> Option<ExtensionFrame<'_>> {
        None
    }

    fn outbound_result(&mut self, _result: Result<(), Error>) {}
}
