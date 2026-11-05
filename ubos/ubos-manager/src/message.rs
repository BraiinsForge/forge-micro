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

//! Wire-level command type for the manager.
//!
//! A [`Message`] is the only thing senders construct, and each variant is
//! a fact or an intent. A sender never spells a `PauseReason`, which is
//! manager-private vocabulary: see `docs/ARCHITECTURE.md` ("Domain
//! messages, not bit-manipulation").

/// Wire-level command sent to the manager via `Handle::send`.
///
/// A condition event carries its edge direction, so the manager does not
/// have to track who asserted what.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum Message {
    /// Cooling: fan tach signal lost or out-of-range.
    BrokenFan,
    /// Cooling: fan tach signal recovered.
    FanRestored,
    /// Tuner: reported a fault it cannot recover from.
    TunerFault,
    /// Tuner: recovered.
    TunerRecovered,
    /// OTA pre-flight: asserts the OTA-update pause before erasing flash.
    OtaBegin,
    /// OTA pre-flight: clears the OTA-update pause after the update completes
    /// or is aborted.
    OtaEnd,
    /// API: operator-issued resume via `POST /api/system/resume`.
    Start,
    /// API: operator-issued pause via `POST /api/system/pause`.
    Stop,
}
