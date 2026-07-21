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
        .assert(PauseReason::TunerFault);
    assert!(all.contains(PauseReason::DangerousTemperature));
    assert!(all.contains(PauseReason::BrokenFan));
    assert!(all.contains(PauseReason::UserRequest));
    assert!(all.contains(PauseReason::PoolUnreachable));
    assert!(all.contains(PauseReason::TunerFault));
}

#[test]
fn as_api_str_returns_camel_case() {
    assert_eq!(
        PauseReason::DangerousTemperature.as_api_str(),
        "dangerousTemperature"
    );
    assert_eq!(PauseReason::BrokenFan.as_api_str(), "brokenFan");
    assert_eq!(PauseReason::UserRequest.as_api_str(), "userRequest");
    assert_eq!(PauseReason::PoolUnreachable.as_api_str(), "poolUnreachable");
    assert_eq!(PauseReason::OtaUpdate.as_api_str(), "otaUpdate");
    assert_eq!(PauseReason::TunerFault.as_api_str(), "tunerFault");
}

#[test]
fn dominant_returns_none_when_empty() {
    assert_eq!(PauseReasons::default().dominant(), None);
}

#[test]
fn dominant_returns_single_active_reason() {
    let reasons = PauseReasons::default().assert(PauseReason::UserRequest);
    assert_eq!(reasons.dominant(), Some(PauseReason::UserRequest));
}

#[test]
fn dominant_prefers_dangerous_temperature_over_all() {
    let reasons = PauseReasons::default()
        .assert(PauseReason::DangerousTemperature)
        .assert(PauseReason::BrokenFan)
        .assert(PauseReason::TunerFault)
        .assert(PauseReason::PoolUnreachable)
        .assert(PauseReason::OtaUpdate)
        .assert(PauseReason::UserRequest);
    assert_eq!(reasons.dominant(), Some(PauseReason::DangerousTemperature));
}

#[test]
fn dominant_priority_order_without_dangerous_temperature() {
    // BrokenFan beats TunerFault, PoolUnreachable, OtaUpdate, UserRequest.
    let reasons = PauseReasons::default()
        .assert(PauseReason::BrokenFan)
        .assert(PauseReason::TunerFault)
        .assert(PauseReason::UserRequest);
    assert_eq!(reasons.dominant(), Some(PauseReason::BrokenFan));

    // TunerFault beats PoolUnreachable, OtaUpdate, UserRequest.
    let reasons = PauseReasons::default()
        .assert(PauseReason::TunerFault)
        .assert(PauseReason::PoolUnreachable)
        .assert(PauseReason::OtaUpdate)
        .assert(PauseReason::UserRequest);
    assert_eq!(reasons.dominant(), Some(PauseReason::TunerFault));

    // UserRequest is lowest priority.
    let reasons = PauseReasons::default().assert(PauseReason::UserRequest);
    assert_eq!(reasons.dominant(), Some(PauseReason::UserRequest));
}
