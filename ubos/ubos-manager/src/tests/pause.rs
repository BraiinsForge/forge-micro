//! Unit tests for the `PauseReasons` bitmap. Plain values, no
//! embassy executor, so the `assert`/`clear`/`contains`/`is_empty`
//! semantics are easy to nail down.

use super::pause::{PauseReason, PauseReasons};

#[test]
fn default_is_empty() {
    let reasons = PauseReasons::default();
    assert!(reasons.is_empty());
    assert!(!reasons.contains(PauseReason::DangerousTemperature));
}

#[test]
fn assert_sets_bit() {
    let reasons = PauseReasons::default().assert(PauseReason::BrokenFan);
    assert!(!reasons.is_empty());
    assert!(reasons.contains(PauseReason::BrokenFan));
    assert!(!reasons.contains(PauseReason::DangerousTemperature));
}

#[test]
fn assert_is_idempotent() {
    let once = PauseReasons::default().assert(PauseReason::UserRequest);
    let twice = once.assert(PauseReason::UserRequest);
    assert_eq!(once, twice);
}

#[test]
fn clear_unsets_bit() {
    let reasons = PauseReasons::default()
        .assert(PauseReason::BrokenFan)
        .clear(PauseReason::BrokenFan);
    assert!(reasons.is_empty());
}

#[test]
fn clear_unset_bit_is_noop() {
    let reasons = PauseReasons::default().clear(PauseReason::PoolUnreachable);
    assert!(reasons.is_empty());
}

#[test]
fn multiple_reasons_coexist() {
    let reasons = PauseReasons::default()
        .assert(PauseReason::DangerousTemperature)
        .assert(PauseReason::BrokenFan);
    assert!(reasons.contains(PauseReason::DangerousTemperature));
    assert!(reasons.contains(PauseReason::BrokenFan));
    assert!(!reasons.is_empty());
}

#[test]
fn clear_one_leaves_others_set() {
    let reasons = PauseReasons::default()
        .assert(PauseReason::DangerousTemperature)
        .assert(PauseReason::BrokenFan)
        .clear(PauseReason::BrokenFan);
    assert!(reasons.contains(PauseReason::DangerousTemperature));
    assert!(!reasons.contains(PauseReason::BrokenFan));
    assert!(!reasons.is_empty());
}

#[test]
fn empty_edge_after_clearing_last_bit() {
    let asserted = PauseReasons::default().assert(PauseReason::TunerFault);
    assert!(!asserted.is_empty());
    let cleared = asserted.clear(PauseReason::TunerFault);
    assert!(cleared.is_empty());
}

#[test]
fn all_variants_occupy_distinct_bits() {
    let all = PauseReasons::default()
        .assert(PauseReason::DangerousTemperature)
        .assert(PauseReason::BrokenFan)
        .assert(PauseReason::UserRequest)
        .assert(PauseReason::PoolUnreachable)
        .assert(PauseReason::Reconfiguring)
        .assert(PauseReason::TunerFault);
    assert!(all.contains(PauseReason::DangerousTemperature));
    assert!(all.contains(PauseReason::BrokenFan));
    assert!(all.contains(PauseReason::UserRequest));
    assert!(all.contains(PauseReason::PoolUnreachable));
    assert!(all.contains(PauseReason::Reconfiguring));
    assert!(all.contains(PauseReason::TunerFault));
}
