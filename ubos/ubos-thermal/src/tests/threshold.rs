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

//! Unit tests for the pure threshold evaluator. Plain values, no
//! embassy executor, so boundary semantics are easy to nail down.

use super::threshold::{Action, evaluate};

const DANGEROUS: f32 = 90.0;
const COOLDOWN: f32 = 45.0;

#[test]
fn ok_below_dangerous() {
    assert_eq!(evaluate(70.0, false, DANGEROUS, COOLDOWN), Action::Ok);
}

#[test]
fn dangerous_halts() {
    assert_eq!(
        evaluate(91.0, false, DANGEROUS, COOLDOWN),
        Action::EnterHalt
    );
}

#[test]
fn dangerous_while_halted_keeps_halt() {
    assert_eq!(evaluate(91.0, true, DANGEROUS, COOLDOWN), Action::Ok);
}

#[test]
fn halted_resumes_under_cooldown() {
    assert_eq!(evaluate(40.0, true, DANGEROUS, COOLDOWN), Action::Resume);
}

#[test]
fn halted_above_cooldown_stays_halted() {
    assert_eq!(evaluate(60.0, true, DANGEROUS, COOLDOWN), Action::Ok);
}

#[test]
fn boundary_at_dangerous_triggers_halt() {
    // Inclusive: 90.0 must already trip the halt, not stay Ok.
    assert_eq!(
        evaluate(90.0, false, DANGEROUS, COOLDOWN),
        Action::EnterHalt
    );
}

#[test]
fn boundary_at_cooldown_resumes() {
    // Inclusive: 45.0 must already clear the halt, not stay Ok.
    assert_eq!(evaluate(45.0, true, DANGEROUS, COOLDOWN), Action::Resume);
}

#[test]
fn halts_at_the_configured_threshold() {
    // The evaluator trusts `dangerous_hashboard_temp_c` straight from the config
    // (the config layer bounds it); there is no separate hard ceiling.
    const HIGH: f32 = 105.0;
    assert_eq!(evaluate(HIGH, false, HIGH, COOLDOWN), Action::EnterHalt);
    assert_eq!(evaluate(HIGH - 1.0, false, HIGH, COOLDOWN), Action::Ok);
}
