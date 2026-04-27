//! Unit tests for the pure threshold evaluator. Plain values, no
//! embassy executor, so boundary semantics are easy to nail down.

use super::threshold::{evaluate, Action, Limits};

const LIMITS: Limits = Limits {
    hot_temp_c: 80.0,
    dangerous_temp_c: 90.0,
    cooldown_temp_c: 45.0,
};

#[test]
fn ok_below_hot() {
    assert_eq!(evaluate(70.0, false, &LIMITS), Action::Ok);
}

#[test]
fn hot_warns_but_does_not_halt() {
    assert_eq!(evaluate(82.0, false, &LIMITS), Action::Hot);
}

#[test]
fn dangerous_halts() {
    assert_eq!(evaluate(91.0, false, &LIMITS), Action::EnterHalt);
}

#[test]
fn dangerous_while_halted_keeps_halt() {
    assert_eq!(evaluate(91.0, true, &LIMITS), Action::Ok);
}

#[test]
fn halted_resumes_under_cooldown() {
    assert_eq!(evaluate(40.0, true, &LIMITS), Action::Resume);
}

#[test]
fn halted_above_cooldown_stays_halted() {
    assert_eq!(evaluate(60.0, true, &LIMITS), Action::Ok);
}

#[test]
fn boundary_at_hot_triggers_hot() {
    // Inclusive: 80.0 must already trip the hot warning, not stay Ok.
    assert_eq!(evaluate(80.0, false, &LIMITS), Action::Hot);
}

#[test]
fn boundary_at_dangerous_triggers_halt() {
    // Inclusive: 90.0 must already trip the halt, not stay Hot.
    assert_eq!(evaluate(90.0, false, &LIMITS), Action::EnterHalt);
}

#[test]
fn boundary_at_cooldown_resumes() {
    // Inclusive: 45.0 must already clear the halt, not stay Ok.
    assert_eq!(evaluate(45.0, true, &LIMITS), Action::Resume);
}
