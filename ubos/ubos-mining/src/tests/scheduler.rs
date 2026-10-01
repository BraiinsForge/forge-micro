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

//! Tests for [`RatioScheduler`]'s dispatch-feedback loop: exact
//! alternation at equal weights, convergence to a configured ratio,
//! bounded catch-up after a simulated outage, weight-0 backup
//! behavior, the all-unavailable case, and `set_weights` truncation,
//! credit reset, and clamp recomputation.
//!
//! Each test drives the same feedback loop `Serial::dispatch_work`
//! will drive: `evaluate` picks a slot, `accumulate_share` credits
//! the slot that was picked.
//!
//! The [`FeeScheduler`] tests that measure the fee share are live only
//! with the fee switch on, the state `test-ubos-mining` in
//! `nix/checks.nix` runs in.

use super::*;

/// Builds a minimal [`ubos_config::PoolEntry`] with the given weight;
/// host/user/protocol are irrelevant to the mapping under test.
fn entry(weight: u32) -> ubos_config::PoolEntry {
    ubos_config::PoolEntry {
        host: heapless::String::new(),
        port: 3333,
        user: heapless::String::new(),
        protocol: ubos_config::ProtocolKind::Sv1 { password: None },
        weight,
    }
}

/// Runs `rounds` iterations of the evaluate/accumulate feedback loop
/// against `available` and returns how many times each slot was
/// picked. Panics if a round has nothing dispatchable, since every
/// test that calls this expects a pick on every round.
fn run<const N: usize>(
    scheduler: &mut impl Scheduler<N>,
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

#[test]
fn pool_weights_maps_entries_to_their_slots() {
    let pool = [entry(5), entry(0), entry(3)];
    assert_eq!(pool_weights(&pool), [5, 0, 3, 0, 0, 0, 0, 0]);
}

#[test]
fn pool_weights_empty_list_is_all_zero() {
    let pool: [ubos_config::PoolEntry; 0] = [];
    assert_eq!(pool_weights(&pool), [0u32; 8]);
}

/// Slots a fee board runs: the operator's and the fee slot.
const FEE_SLOTS: usize = ubos_config::pool::MAX_POOLS + 1;
const FEE_SLOT: usize = FEE_SLOTS - 1;

const ALL: [bool; FEE_SLOTS] = [true; FEE_SLOTS];

const PERMILLE: i64 = 20;

#[test]
fn the_fee_slot_takes_its_share_exactly() {
    if !DEVFEE {
        return;
    }
    let mut scheduler = FeeScheduler::<FEE_SLOTS>::new(PERMILLE);
    let rounds = 100_000;
    let picks = run(&mut scheduler, &ALL, rounds);
    let expected = i64::from(rounds) * PERMILLE / FEE_DEN;
    let fee = i64::from(picks[FEE_SLOT]);
    assert!(
        (fee - expected).abs() <= 1,
        "fee slot got {fee} of {rounds}, expected {expected}"
    );
}

#[test]
fn the_operator_slots_keep_their_ratio() {
    if !DEVFEE {
        return;
    }
    let mut scheduler = FeeScheduler::<FEE_SLOTS>::new(PERMILLE);
    scheduler.set_weights(&[2, 1, 0]);
    let picks = run(&mut scheduler, &ALL, 30_000);
    let [primary, secondary, backup, fee] = picks;
    assert_eq!(backup, 0, "a weight-0 slot is backup only");
    assert!(fee > 0, "the fee slot was never picked");
    let ratio = f64::from(primary) / f64::from(secondary);
    assert!(
        (ratio - 2.0).abs() < 0.01,
        "operator split {primary}:{secondary}"
    );
}

/// Sessions of a board whose operator list fills every slot but the
/// fee's: three, so the operator's third entry lands in the fee slot.
const THREE_SESSIONS: usize = ubos_config::pool::MAX_POOLS;

#[test]
fn the_operator_split_ignores_the_entry_in_the_fee_slot() {
    if !DEVFEE {
        return;
    }
    let mut scheduler = FeeScheduler::<THREE_SESSIONS>::new(PERMILLE);
    scheduler.set_weights(&[2, 1, 1]);
    let picks = run(&mut scheduler, &[true; THREE_SESSIONS], 30_000);
    let [primary, secondary, fee] = picks;
    assert!(fee > 0, "the fee slot was never picked");
    let ratio = f64::from(primary) / f64::from(secondary);
    assert!(
        (ratio - 2.0).abs() < 0.01,
        "operator split {primary}:{secondary}"
    );
}

#[test]
fn no_operator_pool_means_no_mining() {
    if !DEVFEE {
        return;
    }
    let mut scheduler = FeeScheduler::<FEE_SLOTS>::new(PERMILLE);
    let mut fee_only = [false; FEE_SLOTS];
    fee_only[FEE_SLOT] = true;
    // Even with a fee share due, the fee slot is not a backup.
    for _ in 0..100 {
        scheduler.accumulate_share(0);
    }
    assert_eq!(scheduler.evaluate(&fee_only), None);
}

#[test]
fn a_dead_fee_pool_falls_through_to_the_operator() {
    if !DEVFEE {
        return;
    }
    let mut scheduler = FeeScheduler::<FEE_SLOTS>::new(PERMILLE);
    let mut operator_only = ALL;
    operator_only[FEE_SLOT] = false;
    let picks = run(&mut scheduler, &operator_only, 1_000);
    assert_eq!(picks[FEE_SLOT], 0);
    assert_eq!(picks[0], 1_000);
    // The debt the outage built is repaid once the fee pool returns.
    assert_eq!(scheduler.evaluate(&ALL), Some(FEE_SLOT));
}

/// A scheduler with the debt at the cap: the fee pool was down for far
/// more operator shares than the cap is worth.
fn after_a_long_outage() -> FeeScheduler<FEE_SLOTS> {
    let mut scheduler = FeeScheduler::<FEE_SLOTS>::new(PERMILLE);
    let mut operator_only = ALL;
    operator_only[FEE_SLOT] = false;
    let rounds = u32::try_from(CAP_FEE_SHARES * FEE_DEN / PERMILLE).expect("fits");
    let _ = run(&mut scheduler, &operator_only, rounds * 2);
    scheduler
}

/// Fee shares in `rounds` picks with every slot up, and the longest
/// run of fee picks in a row.
fn fee_picks(scheduler: &mut FeeScheduler<FEE_SLOTS>, rounds: u32) -> (i64, i64) {
    let (mut fee, mut streak, mut longest) = (0, 0, 0);
    for _ in 0..rounds {
        let pick = scheduler.evaluate(&ALL).expect("an operator pool is up");
        scheduler.accumulate_share(pick);
        if pick == FEE_SLOT {
            fee += 1;
            streak += 1;
            longest = longest.max(streak);
        } else {
            streak = 0;
        }
    }
    (fee, longest)
}

#[test]
fn an_outage_is_repaid_up_to_the_cap() {
    if !DEVFEE {
        return;
    }
    let mut scheduler = after_a_long_outage();
    // Each repayment cycle is one fee share and REPAY_SPACING operator
    // shares, which add to the debt again, so the cap takes a little
    // more than CAP_FEE_SHARES fee shares to clear.
    let due = FEE_DEN - PERMILLE;
    let cycles = CAP_FEE_SHARES * due / (due - REPAY_SPACING * PERMILLE);
    let rounds = u32::try_from(cycles * (REPAY_SPACING + 1)).expect("fits");
    let (repaid, _) = fee_picks(&mut scheduler, rounds);
    assert!(
        (repaid - cycles).abs() <= 2,
        "repaid {repaid} fee shares, expected {cycles}"
    );
    // Then the steady share and nothing more.
    let (steady, _) = fee_picks(&mut scheduler, 10_000);
    let expected = 10_000 * PERMILLE / FEE_DEN;
    assert!((steady - expected).abs() <= 2, "after the cap: {steady}");
}

#[test]
fn a_repayment_alternates_with_the_operator() {
    if !DEVFEE {
        return;
    }
    let mut scheduler = after_a_long_outage();
    let (fee, longest) = fee_picks(&mut scheduler, 2_000);
    assert!(fee >= 999, "the fee took only {fee} of 2000 while owed");
    assert_eq!(longest, 1, "the fee took {longest} picks in a row");
}

#[test]
#[should_panic(expected = "cannot repay a debt")]
fn a_fee_share_of_half_the_work_cannot_repay_a_debt() {
    let _ = FeeScheduler::<FEE_SLOTS>::new(FEE_DEN / (REPAY_SPACING + 1));
}

#[test]
fn the_debt_survives_a_reweight() {
    if !DEVFEE {
        return;
    }
    let mut scheduler = FeeScheduler::<FEE_SLOTS>::new(PERMILLE);
    let shares_per_fee = u32::try_from((FEE_DEN - PERMILLE) / PERMILLE).expect("fits");
    for _ in 0..shares_per_fee {
        scheduler.accumulate_share(0);
    }
    scheduler.set_weights(&[1, 1, 0]);
    assert_eq!(scheduler.evaluate(&ALL), Some(FEE_SLOT));
}

#[test]
fn a_zero_share_never_picks_the_fee_slot() {
    if !DEVFEE {
        return;
    }
    let mut scheduler = FeeScheduler::<FEE_SLOTS>::new(0);
    let picks = run(&mut scheduler, &ALL, 10_000);
    assert_eq!(picks[FEE_SLOT], 0);
}

#[test]
#[should_panic(expected = "a fee share is a number from 0 to 999")]
fn a_whole_share_is_refused() {
    let _ = FeeScheduler::<FEE_SLOTS>::new(FEE_DEN);
}

// The fee needs an operator slot beside it, so this holds whatever the
// switch.
#[test]
fn a_one_slot_board_runs_the_operator_split_alone() {
    let mut fee = FeeScheduler::<1>::new(PERMILLE);
    let mut plain = RatioScheduler::<1>::new();
    fee.set_weights(&[2]);
    plain.set_weights(&[2]);
    let all = [true; 1];
    assert_eq!(run(&mut fee, &all, 3_000), run(&mut plain, &all, 3_000));
}
