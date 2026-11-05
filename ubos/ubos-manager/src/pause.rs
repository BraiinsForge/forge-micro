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

//! Pause reason types and bitmask type.
//!
//! Both are manager-private vocabulary: only `run_manager` constructs
//! them, from the [`Message`]s it receives. The dangerous temperature
//! flag of `ubos-thermal` and the unreadable temperature rail cut of
//! `ubos-mining` halt mining without passing through here.

use crate::message::Message;

/// Every condition that causes the manager to halt mining.
///
/// Each variant maps to one bit in [`PauseReasons`], and any non-empty
/// mask means mining must be halted; there is no observation-only pause
/// reason.
///
/// The discriminants are wire-format bit positions, so they must not be
/// renumbered.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum PauseReason {
    /// Fan tachometer signal lost or out-of-range.
    BrokenFan = 1,
    /// Operator-requested halt via `POST /api/system/pause`.
    UserRequest = 2,
    /// An over-the-air firmware update is in progress and is erasing flash.
    OtaUpdate = 4,
    /// The tuner reported a fault it cannot recover from.
    TunerFault = 5,
}

impl PauseReason {
    /// Rank among the reasons, most severe first, and the order
    /// [`PRIORITY`] lists them in. A match, so that a new variant does
    /// not compile until it is ranked.
    const fn severity(self) -> usize {
        match self {
            Self::BrokenFan => 0,
            Self::TunerFault => 1,
            Self::OtaUpdate => 2,
            Self::UserRequest => 3,
        }
    }
}

/// Live bitmask of active pause reasons. Positions are hidden behind the
/// `assert`/`clear`/`contains`/`is_empty` API, so a caller never
/// manipulates bits directly.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct PauseReasons(u16);

impl PauseReasons {
    /// Apply a `Message` to this mask, returning the new mask.
    ///
    /// Public so that a host test in a consuming crate can stand in for
    /// the manager without a second copy of [`effect`].
    #[must_use]
    pub fn apply(self, message: Message) -> Self {
        let (reason, asserted) = effect(message);
        if asserted {
            self.assert(reason)
        } else {
            self.clear(reason)
        }
    }

    #[must_use]
    pub fn assert(self, reason: PauseReason) -> Self {
        Self(self.0 | (1u16 << reason as u8))
    }

    #[must_use]
    pub fn clear(self, reason: PauseReason) -> Self {
        Self(self.0 & !(1u16 << reason as u8))
    }

    /// True when mining may run.
    #[must_use]
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    #[must_use]
    pub fn contains(self, reason: PauseReason) -> bool {
        self.0 & (1u16 << reason as u8) != 0
    }

    /// The highest-priority active reason, or `None` when the mask is
    /// empty. The order is [`PauseReason::severity`]. The overheat halt is
    /// the flag of `ubos-thermal`, and the API reports it in front of every
    /// reason in this mask.
    #[must_use]
    pub fn dominant(self) -> Option<PauseReason> {
        PRIORITY.into_iter().find(|&r| self.contains(r))
    }
}

/// The one bit a message moves, and whether it asserts or clears it.
/// This is the whole of the manager's interpretation.
pub(crate) const fn effect(message: Message) -> (PauseReason, bool) {
    match message {
        Message::BrokenFan => (PauseReason::BrokenFan, true),
        Message::FanRestored => (PauseReason::BrokenFan, false),
        Message::TunerFault => (PauseReason::TunerFault, true),
        Message::TunerRecovered => (PauseReason::TunerFault, false),
        Message::OtaBegin => (PauseReason::OtaUpdate, true),
        Message::OtaEnd => (PauseReason::OtaUpdate, false),
        Message::Stop => (PauseReason::UserRequest, true),
        Message::Start => (PauseReason::UserRequest, false),
    }
}

/// Every [`PauseReason`] in [`PauseReason::severity`] order, which is
/// what [`PauseReasons::dominant`] walks.
const PRIORITY: [PauseReason; REASON_COUNT] = [
    PauseReason::BrokenFan,
    PauseReason::TunerFault,
    PauseReason::OtaUpdate,
    PauseReason::UserRequest,
];

/// How many reasons exist. The match is exhaustive, so a new variant does
/// not compile until it is added here and the count moves with it, which
/// in turn fails [`PRIORITY`]'s array literal until the new reason is
/// ranked. A hand-written count would catch neither.
const REASON_COUNT: usize = match PauseReason::UserRequest {
    PauseReason::BrokenFan
    | PauseReason::TunerFault
    | PauseReason::OtaUpdate
    | PauseReason::UserRequest => 4,
};

// `severity` and `REASON_COUNT` already rule out an unranked or missing
// entry. What is left for this block is the order: every entry standing
// at its own rank rules out a duplicate and a swap, either of which would
// show the operator the wrong cause.
const _: () = {
    let mut i = 0;
    while i < PRIORITY.len() {
        assert!(
            PRIORITY[i].severity() == i,
            "PRIORITY is not in severity order"
        );
        i += 1;
    }
};
