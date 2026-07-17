//! Tests for [`RatioScheduler`]'s dispatch-feedback loop: exact
//! alternation at equal weights, convergence to a configured ratio,
//! bounded catch-up after a simulated outage, weight-0 backup
//! behavior, the all-unavailable case, and `set_weights` truncation,
//! credit reset, and clamp recomputation.
//!
//! Each test drives the same feedback loop `Serial::dispatch_work`
//! will drive: `evaluate` picks a slot, `accumulate_share` credits
//! the slot that was picked.

use super::*;

/// Runs `rounds` iterations of the evaluate/accumulate feedback loop
/// against `available` and returns how many times each slot was
/// picked. Panics if a round has nothing dispatchable, since every
/// test that calls this expects a pick on every round.
fn run<const N: usize>(
    scheduler: &mut RatioScheduler<N>,
    available: &[bool; N],
    rounds: u32,
) -> [u32; N] {
    let mut picks = [0u32; N];
    for _ in 0..rounds {
        let slot = scheduler
            .evaluate(available)
            .expect("test round expects a dispatchable slot");
        picks[slot] += 1;
        scheduler.accumulate_share(slot);
    }
    picks
}

#[test]
fn alternates_every_share_at_1_1() {
    let mut scheduler = RatioScheduler::<2>::new();
    scheduler.set_weights(&[1, 1]);
    let available = [true, true];
    for round in 0..100 {
        let slot = scheduler
            .evaluate(&available)
            .expect("both slots available");
        let expected = round % 2;
        assert_eq!(slot, expected, "round {round} picked the wrong slot");
        scheduler.accumulate_share(slot);
    }
}

#[test]
fn converges_to_2_1() {
    let mut scheduler = RatioScheduler::<2>::new();
    scheduler.set_weights(&[2, 1]);
    let picks = run(&mut scheduler, &[true, true], 9_999);
    let expected = 6_666;
    let tolerance = 67;
    assert!(
        picks[0].abs_diff(expected) <= tolerance,
        "slot 0 picked {} times, expected {expected} +/- {tolerance}",
        picks[0]
    );
}

#[test]
fn outage_catchup_is_bounded() {
    let mut scheduler = RatioScheduler::<2>::new();
    scheduler.set_weights(&[1, 1]);
    // Slot 1 is down long enough for both credits to saturate at
    // the clamp: slot 0 at -CLAMP, slot 1 at +CLAMP.
    run(&mut scheduler, &[true, false], 10_000);

    // Once slot 1 comes back, it is owed its saturated credit and
    // gets picked exclusively until the debt is repaid.
    let available = [true, true];
    let mut consecutive_slot_1 = 0;
    loop {
        let slot = scheduler
            .evaluate(&available)
            .expect("both slots available");
        if slot == 0 {
            break;
        }
        consecutive_slot_1 += 1;
        scheduler.accumulate_share(slot);
    }

    assert!(
        consecutive_slot_1 >= CLAMP_SHARES,
        "catch-up too short: {consecutive_slot_1} < {CLAMP_SHARES}"
    );
    assert!(
        consecutive_slot_1 <= 2 * CLAMP_SHARES,
        "catch-up not bounded: {consecutive_slot_1} > {}",
        2 * CLAMP_SHARES
    );
}

#[test]
fn weight_zero_is_backup_only() {
    let mut scheduler = RatioScheduler::<2>::new();
    scheduler.set_weights(&[1, 0]);

    for _ in 0..10 {
        let slot = scheduler
            .evaluate(&[true, true])
            .expect("slot 0 is always dispatchable");
        assert_eq!(slot, 0, "weighted slot must be preferred while it works");
        scheduler.accumulate_share(slot);
    }

    let slot = scheduler
        .evaluate(&[false, true])
        .expect("slot 1 is the only available slot");
    assert_eq!(slot, 1, "backup slot must take over while slot 0 is down");
    scheduler.accumulate_share(slot);

    let slot = scheduler
        .evaluate(&[true, true])
        .expect("slot 0 is dispatchable again");
    assert_eq!(slot, 0, "weighted slot must resume once it recovers");
}

#[test]
fn all_unavailable_returns_none() {
    let mut scheduler = RatioScheduler::<2>::new();
    scheduler.set_weights(&[1, 1]);
    assert_eq!(scheduler.evaluate(&[false, false]), None);
}

#[test]
fn new_defaults_to_slot_0() {
    let mut scheduler = RatioScheduler::<3>::new();
    let available = [true, true, true];
    for round in 0..10 {
        let slot = scheduler
            .evaluate(&available)
            .expect("slot 0 is always dispatchable by default");
        assert_eq!(
            slot, 0,
            "round {round}: fresh scheduler must default to slot 0"
        );
        scheduler.accumulate_share(slot);
    }
}

#[test]
fn set_weights_truncates_and_falls_back_to_slot_0() {
    let mut scheduler = RatioScheduler::<2>::new();
    // The weighted entry lies beyond this scheduler's slot count, so
    // the truncated set is all zero and the post-truncation fallback
    // applies.
    scheduler.set_weights(&[0, 0, 5]);

    let slot = scheduler
        .evaluate(&[true, true])
        .expect("slot 0 is dispatchable");
    assert_eq!(slot, 0, "post-truncation fallback must mine slot 0");
}

#[test]
fn set_weights_resets_credits_for_a_clean_convergence() {
    let mut scheduler = RatioScheduler::<2>::new();
    scheduler.set_weights(&[1, 1]);
    run(&mut scheduler, &[true, true], 300);

    scheduler.set_weights(&[9, 1]);
    let picks = run(&mut scheduler, &[true, true], 10_000);

    let expected = 9_000;
    let tolerance = 100; // 1% of 10_000 rounds
    assert!(
        picks[0].abs_diff(expected) <= tolerance,
        "slot 0 picked {} times after reweight, expected {expected} +/- {tolerance}",
        picks[0]
    );
}

#[test]
fn set_weights_recomputes_the_clamp() {
    let mut scheduler = RatioScheduler::<2>::new();
    scheduler.set_weights(&[1, 1]);
    scheduler.set_weights(&[2, 1]);

    // Slot 1 is down long enough for both credits to saturate at the
    // clamp derived from the new weight sum.
    run(&mut scheduler, &[true, false], 10_000);

    let available = [true, true];
    let mut consecutive_slot_1 = 0;
    loop {
        let slot = scheduler
            .evaluate(&available)
            .expect("both slots available");
        if slot == 0 {
            break;
        }
        consecutive_slot_1 += 1;
        scheduler.accumulate_share(slot);
    }

    assert!(
        consecutive_slot_1 >= CLAMP_SHARES,
        "catch-up too short after reweight: {consecutive_slot_1} < {CLAMP_SHARES}"
    );
    assert!(
        consecutive_slot_1 <= 2 * CLAMP_SHARES,
        "catch-up not bounded after reweight: {consecutive_slot_1} > {}",
        2 * CLAMP_SHARES
    );
}
