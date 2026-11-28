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

//! Tests for [`ChainStats`] and [`ChainStatsHandle`].

use super::*;
extern crate std;
use std::boxed::Box;

use super::ChainPresence;

/// The leading-zero bits of a difficulty-one target.
const DIFF_1_BITS: u32 = 32;

/// A target of power-of-two difficulty `2^log2`, built from its
/// leading-zero count as the drivers' targets are.
fn pow2_target(log2: u32) -> [u8; 32] {
    let bits = DIFF_1_BITS + log2;
    let mut target = [0u8; 32];
    target[31 - (bits / 8) as usize] = 0x80 >> (bits % 8);
    target
}

/// The difficulty-one target, so a solution against it counts one
/// share in every counter these tests read.
fn some_target() -> [u8; 32] {
    pow2_target(0)
}

#[test]
fn record_share_bumps_share_count_and_pool_slot() {
    let stats = ChainStats::new();
    stats.record_share(&some_target(), 2);

    assert_eq!(stats.share_count().get(), 1);
    assert_eq!(stats.pool_shares(2).get(), 1);
    assert_eq!(stats.pool_shares(0).get(), 0);
}

// A solution found against a harder filter counts as that many shares in
// both counters, so a counter times 2^32 stays the hash count.
#[test]
fn record_share_weighs_a_solution_by_its_difficulty() {
    let stats = ChainStats::new();
    stats.record_share(&pow2_target(10), 1);
    stats.record_share(&some_target(), 1);

    assert_eq!(stats.share_count().get(), 1024 + 1);
    assert_eq!(stats.pool_shares(1).get(), 1024 + 1);
}

#[test]
fn record_share_out_of_range_slot_still_counts_share() {
    let stats = ChainStats::new();
    stats.record_share(&some_target(), 99);

    assert_eq!(stats.share_count().get(), 1);
    for slot in 0..MAX_POOL_SLOTS {
        assert_eq!(stats.pool_shares(slot).get(), 0);
    }
    assert_eq!(stats.pool_shares(99).get(), 0);
}

#[test]
fn active_pool_round_trips_and_rejects_out_of_range_slots() {
    let stats = ChainStats::new();
    assert_eq!(stats.active_pool(), None);

    stats.set_active_pool(Some(2));
    assert_eq!(stats.active_pool(), Some(2));

    stats.set_active_pool(Some(MAX_POOL_SLOTS));
    assert_eq!(stats.active_pool(), None);

    stats.set_active_pool(Some(0));
    stats.set_active_pool(None);
    assert_eq!(stats.active_pool(), None);
}

#[test]
fn clear_resets_bring_up_fields_but_not_lifetime_counters() {
    let stats = ChainStats::new();
    stats.record_share(&some_target(), 0);
    stats.set_presence(ChainPresence::Present { chips: 120 });
    stats.set_frequency(Frequency::from_khz(500_000));
    stats.set_active_pool(Some(1));

    stats.clear();

    assert_eq!(
        stats.share_count().get(),
        1,
        "clear must not touch share_count"
    );
    assert_eq!(
        stats.pool_shares(0).get(),
        1,
        "clear must not touch pool_shares"
    );
    assert_eq!(stats.chips(), 0);
    assert_eq!(stats.frequency().khz(), 0);
    assert_eq!(stats.active_pool(), None);
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

/// A three-slot table with `active` leading slots wired.
fn table(active: u8) -> ChainStatsHandle {
    let stats: &'static [ChainStats; 3] = Box::leak(Box::new([const { ChainStats::new() }; 3]));
    let active: &'static AtomicU8 = Box::leak(Box::new(AtomicU8::new(active)));
    ChainStatsHandle::new(stats, active)
}

#[test]
fn handle_walks_no_slot_before_the_chain_count_is_published() {
    let handle = table(0);
    handle
        .chain(HashboardIndex(0))
        .record_share(&some_target(), 0);

    assert_eq!(handle.slots(), 3);
    assert_eq!(handle.chain_count(), 0);
    assert_eq!(handle.chains().count(), 0);
    assert_eq!(handle.machine_hashrate().hps(), 0);
    assert_eq!(handle.machine_share_count().get(), 0);
}

#[test]
fn handle_walks_only_the_wired_prefix() {
    let handle = table(2);
    handle
        .chain(HashboardIndex(2))
        .set_presence(ChainPresence::Present { chips: 12 });
    handle
        .chain(HashboardIndex(2))
        .record_share(&some_target(), 0);

    assert_eq!(handle.chain_count(), 2);
    assert_eq!(handle.chains().count(), 2);
    assert_eq!(handle.present_chain_count(), 0);
    assert_eq!(handle.machine_share_count().get(), 0);
}

#[test]
fn handle_clamps_the_chain_count_to_the_table() {
    let handle = table(u8::MAX);

    assert_eq!(handle.chain_count(), 3);
    assert_eq!(handle.chains().count(), 3);
}

#[test]
fn present_chain_count_counts_only_present() {
    let handle = table(3);
    handle
        .chain(HashboardIndex(0))
        .set_presence(ChainPresence::Present { chips: 12 });
    handle
        .chain(HashboardIndex(1))
        .set_presence(ChainPresence::Absent);
    handle
        .chain(HashboardIndex(2))
        .set_presence(ChainPresence::Faulted { found: 5 });

    assert_eq!(handle.present_chain_count(), 1);
}

#[test]
fn share_counts_sum_across_chains() {
    let handle = table(3);
    for _ in 0..3 {
        handle
            .chain(HashboardIndex(0))
            .record_share(&some_target(), 0);
    }
    for _ in 0..5 {
        handle
            .chain(HashboardIndex(1))
            .record_share(&some_target(), 0);
    }

    assert_eq!(handle.machine_share_count().get(), 8);
    assert_eq!(handle.chain(HashboardIndex(0)).share_count().get(), 3);
    assert_eq!(handle.chain(HashboardIndex(1)).share_count().get(), 5);
    assert_eq!(handle.chain(HashboardIndex(2)).share_count().get(), 0);
}

#[test]
fn desync_bytes_saturate_instead_of_wrapping() {
    let stats = ChainStats::new();
    stats.add_desync_bytes(u32::MAX - 1);
    stats.add_desync_bytes(8);
    assert_eq!(stats.desync_bytes(), u32::MAX);
}
