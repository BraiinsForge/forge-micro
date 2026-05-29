//! Unit tests for the pure threshold evaluator. Plain values, no
//! embassy executor, so boundary semantics are easy to nail down.

use super::threshold::{evaluate, Action};

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
