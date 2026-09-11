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

//! Tests for [`ChainStats`]: share and pool-slot accounting, the
//! temperature sentinel round trip, `clear`'s lifetime-counter
//! exemption, presence storage, and summing share counts across
//! chains.

use super::*;
use crate::presence::ChainPresence;

/// An arbitrary target used only to drive `record_share`; the exact
/// leading-zero count is irrelevant to these tests, which check the
/// share and pool-slot counters rather than the hashrate estimate.
fn some_target() -> [u8; 32] {
    let mut target = [0u8; 32];
    target[0] = 0x01;
    target
}

#[test]
fn record_share_bumps_share_count_and_pool_slot() {
    let stats = ChainStats::new();
    stats.record_share(&some_target(), 2);

    assert_eq!(stats.share_count(), 1);
    assert_eq!(stats.pool_shares(2), 1);
    assert_eq!(stats.pool_shares(0), 0);
}

#[test]
fn record_share_out_of_range_slot_still_counts_share() {
    let stats = ChainStats::new();
    stats.record_share(&some_target(), 99);

    assert_eq!(stats.share_count(), 1);
    for slot in 0..MAX_POOL_SLOTS {
        assert_eq!(stats.pool_shares(slot), 0);
    }
    assert_eq!(stats.pool_shares(99), 0);
}

#[test]
fn hottest_mc_round_trips_none_and_some() {
    let stats = ChainStats::new();
    assert_eq!(stats.hottest_mc(), None);

    stats.set_hottest(Some(42));
    assert_eq!(stats.hottest_mc(), Some(42));

    stats.set_hottest(None);
    assert_eq!(stats.hottest_mc(), None);
}

#[test]
fn clear_resets_bring_up_fields_but_not_lifetime_counters() {
    let stats = ChainStats::new();
    stats.record_share(&some_target(), 0);
    stats.set_presence(ChainPresence::Present { chips: 120 });
    stats.set_frequency(Frequency::from_khz(500_000));
    stats.set_hottest(Some(85));

    stats.clear();

    assert_eq!(stats.share_count(), 1, "clear must not touch share_count");
    assert_eq!(stats.pool_shares(0), 1, "clear must not touch pool_shares");
    assert_eq!(stats.chips(), 0);
    assert_eq!(stats.frequency().khz(), 0);
    assert_eq!(stats.hottest_mc(), None);
    assert_eq!(
        stats.presence(),
        ChainPresence::Unknown,
        "clear must reset presence"
    );
}

#[test]
fn presence_round_trips_and_updates_chips() {
    let stats = ChainStats::new();
    assert_eq!(stats.presence(), ChainPresence::Unknown);

    stats.set_presence(ChainPresence::Present { chips: 120 });
    assert_eq!(stats.presence(), ChainPresence::Present { chips: 120 });
    assert_eq!(stats.chips(), 120);

    stats.set_presence(ChainPresence::Absent);
    assert_eq!(stats.presence(), ChainPresence::Absent);
    assert_eq!(stats.chips(), 0);

    stats.set_presence(ChainPresence::Faulted { found: 3 });
    assert_eq!(stats.presence(), ChainPresence::Faulted { found: 3 });
    assert_eq!(stats.chips(), 0);
}

#[test]
fn present_chains_counts_only_present() {
    let present = ChainStats::new();
    present.set_presence(ChainPresence::Present { chips: 12 });
    let absent = ChainStats::new();
    absent.set_presence(ChainPresence::Absent);
    let faulted = ChainStats::new();
    faulted.set_presence(ChainPresence::Faulted { found: 5 });

    let chains = [present, absent, faulted];
    let present_count = chains.iter().filter(|c| c.presence().is_present()).count();
    assert_eq!(present_count, 1);
}

#[test]
fn share_counts_sum_across_chains() {
    let first = ChainStats::new();
    let second = ChainStats::new();

    for _ in 0..3 {
        first.record_share(&some_target(), 0);
    }
    for _ in 0..5 {
        second.record_share(&some_target(), 0);
    }

    let summed = first.share_count() + second.share_count();
    assert_eq!(summed, 8);
    assert_eq!(first.share_count(), 3);
    assert_eq!(second.share_count(), 5);
}
