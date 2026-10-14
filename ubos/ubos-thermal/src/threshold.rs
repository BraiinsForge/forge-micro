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

//! Pure thermal-threshold evaluator.
//!
//! Splits dangerous/cooldown decisions out of the embassy task so they
//! can be unit-tested with plain values, with no embassy executor or
//! atomics in the loop.

use ubos_core::units::Temperature;

/// What the thermal task should do for the current sample.
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    /// Below `dangerous`, or above `cooldown` while halted: nothing to do.
    Ok,
    /// Crossed `dangerous`: halt mining via `MiningHandle::halt` and
    /// log an error. Only returned on the rising edge into the halt
    /// state.
    EnterHalt,
    /// Below `cooldown` while halted: resume mining via
    /// `MiningHandle::resume` and log a recovery message.
    Resume,
}

/// Decide what action to take given the current sample and whether the
/// halt flag is currently set.
///
/// `dangerous` is the halt threshold `judge_dangerous` resolved for the
/// source of this reading; the config layer bounds both thresholds it can
/// be (`ThermalConfig::validate` caps them), so there is no separate
/// ceiling here.
#[must_use]
pub fn evaluate(
    temp: Temperature,
    halted: bool,
    dangerous: Temperature,
    cooldown: Temperature,
) -> Action {
    if halted {
        if temp <= cooldown {
            return Action::Resume;
        }
        return Action::Ok;
    }
    if temp >= dangerous {
        return Action::EnterHalt;
    }
    Action::Ok
}
