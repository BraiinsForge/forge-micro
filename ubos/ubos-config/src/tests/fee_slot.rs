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

use super::*;
use crate::Handle;
use crate::fee::{FeePool, FeeSlot, FeeWorker, Fixed};
use crate::tests_fixture::{BOUNDS, default_config};
use core::fmt::Write as _;
use embassy_futures::block_on;
use rusty_fork::rusty_fork_test;

/// Fixed, so the fee entry reads nothing from the operator's pools.
static WORKER: Fixed = Fixed::new("fee.worker");

const FEE: FeePool = FeePool {
    host: "fee.example.com",
    port: 4444,
    pubkey: None,
    worker: &WORKER,
    permille: 20,
};

const FOUR_SESSIONS: usize = MAX_POOLS + 1;

rusty_fork_test! {
    #[test]
    fn an_off_build_places_no_fee_and_runs_the_operator_slots_alone() {
        if DEVFEE {
            return;
        }
        assert_eq!(POOL_SLOTS, MAX_POOLS);
        assert_eq!(FeeSlot::last_of(FEE, FOUR_SESSIONS), None);
        let (runner, handle) = Handle::take();
        runner.publish(default_config());
        assert_eq!(block_on(handle.pool_slot(MAX_POOLS)), None);
        assert_eq!(block_on(handle.view()).fee, None);
        assert_eq!(block_on(handle.view()).fee_permille, None);
    }
}

#[test]
fn the_fee_is_the_last_slot_of_the_board() {
    if !DEVFEE {
        return;
    }
    let four = FeeSlot::last_of(FEE, FOUR_SESSIONS).expect("the switch is on");
    assert_eq!(four.slot, FOUR_SESSIONS - 1);
    let three = FeeSlot::last_of(FEE, MAX_POOLS).expect("the switch is on");
    assert_eq!(three.slot, MAX_POOLS - 1);
    assert_eq!(FeeSlot::index(&Some(three)), Some(MAX_POOLS - 1));
    assert_eq!(FeeSlot::index(&None), None);
}

rusty_fork_test! {
    #[test]
    fn the_handle_reads_the_operator_list_below_the_fee_and_the_fee_at_its_slot() {
        if !DEVFEE {
            return;
        }
        let fee = FeeSlot::last_of(FEE, FOUR_SESSIONS).expect("the switch is on");
        let (runner, handle) = Handle::take();
        crate::fee::FEE_SLOT.init(fee).expect("BUG: the cell is this test's own");
        runner.publish(default_config());
        let config = block_on(handle.current());
        assert_eq!(block_on(handle.pool_slot(0)), config.pool.first().cloned());
        assert_eq!(block_on(handle.pool_slot(MAX_POOLS - 1)), None);
        assert_eq!(block_on(handle.pool_slot(FOUR_SESSIONS - 1)), Some(FEE.entry(&[])));
        assert_eq!(block_on(handle.pool_slot(FOUR_SESSIONS)), None);
        let view = block_on(handle.view());
        assert_eq!(view.pool.as_ref(), Some(&config.pool));
        assert_eq!(view.fee, Some(FEE.entry(&[])));
        assert_eq!(view.fee_permille, Some(FEE.permille));
        assert_eq!(view.validate(&config, &BOUNDS), Ok(()));
    }
}

/// A worker that answers with how many operator pools it was handed,
/// so a test sees which slots reach it.
#[derive(Debug)]
struct Counting;

impl FeeWorker for Counting {
    fn worker(&self, operator: &[PoolEntry]) -> heapless::String<NAME_CAPACITY> {
        let mut count = heapless::String::new();
        write!(count, "{}", operator.len()).expect("fits");
        count
    }
}

rusty_fork_test! {
    // The fee worker is handed the slots below the fee's, which are
    // the ones the board runs: on a three-session board the fee
    // shadows the third configured pool.
    #[test]
    fn the_fee_worker_sees_the_operator_slots_the_board_runs() {
        if !DEVFEE {
            return;
        }
        let fee = FeeSlot::last_of(
            FeePool {
                worker: &Counting,
                ..FEE
            },
            MAX_POOLS,
        )
        .expect("the switch is on");
        let (runner, handle) = Handle::take();
        crate::fee::FEE_SLOT.init(fee).expect("BUG: the cell is this test's own");
        runner.publish(default_config());
        let mut config = block_on(handle.current());
        let primary = config
            .pool
            .first()
            .cloned()
            .expect("the default config has a pool");
        while config.pool.push(primary.clone()).is_ok() {}
        assert_eq!(config.pool.len(), MAX_POOLS);
        crate::STATE_WATCH.sender().send(config);
        let fee_entry = block_on(handle.pool_slot(MAX_POOLS - 1)).expect("the fee slot");
        assert_eq!(fee_entry.user.as_str(), "2");
        assert_eq!(block_on(handle.view()).fee, Some(fee_entry));
    }
}

rusty_fork_test! {
    #[test]
    fn a_three_session_board_runs_the_fee_at_its_third_slot() {
        if !DEVFEE {
            return;
        }
        let fee = FeeSlot::last_of(FEE, MAX_POOLS).expect("the switch is on");
        let (runner, handle) = Handle::take();
        crate::fee::FEE_SLOT.init(fee).expect("BUG: the cell is this test's own");
        runner.publish(default_config());
        assert_eq!(block_on(handle.pool_slot(MAX_POOLS - 1)), Some(FEE.entry(&[])));
        assert_eq!(block_on(handle.pool_slot(MAX_POOLS)), None);
    }
}
