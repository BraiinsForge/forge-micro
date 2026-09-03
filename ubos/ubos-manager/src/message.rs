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
//! `Message` is the only thing senders construct. Each variant
//! is a fact or intent — the manager interprets the message into a
//! `PauseReason` bit transition internally. Senders never spell
//! `PauseReason`; it is manager-private vocabulary.
//!
//! See `docs/ARCHITECTURE.md` ("Domain messages, not
//! bit-manipulation").

/// Wire-level command sent to the manager via `Handle::send`.
///
/// Condition events from other modules carry explicit edge direction
/// so the manager does not have to track who-asserted-what. Operator
/// intent from the API uses `Start`/`Stop`.
#[derive(Clone, Copy)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum Message {
    /// Thermal: chip-max crossed the dangerous threshold.
    DangerousTemperature,
    /// Thermal: chip-max returned below the cooldown threshold (or the
    /// sensor recovered after a fault).
    TemperatureNormal,
    /// Cooling: fan tach signal lost or out-of-range.
    BrokenFan,
    /// Cooling: fan tach signal recovered.
    FanRestored,
    /// Stratum: every configured pool entry is unreachable.
    PoolUnreachable,
    /// Stratum: at least one pool entry reconnected.
    PoolReachable,
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
