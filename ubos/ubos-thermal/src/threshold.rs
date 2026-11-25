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

//! The threshold decision, apart from the task so it tests on plain
//! values.

use ubos_core::units::Temperature;

#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    Ok,
    /// The rising edge into the halt only.
    EnterHalt,
    /// Below `cooldown` while halted.
    Resume,
}

/// `dangerous` is the threshold `judge_dangerous` resolved for this
/// reading's source, and the config layer bounds both it can be, so
/// there is no second ceiling here.
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
