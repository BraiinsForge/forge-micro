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

//! Per-board metric publishers for stratum-client state.
//!
//! Lives here rather than each firmware's `main.rs` so the
//! atomics, metric names, and cadence stay in one place. Boards
//! spawn `run_stratum_metrics_publisher` from their own
//! `#[embassy_executor::task]` wrapper.

use embassy_time::{Duration, Ticker};
use ubos_metrics::metrics::{Value, push_slot_metric};

use crate::run::POOL_INDEX_SPACE;

/// Periodically push `board_shares_accepted_total`,
/// `board_shares_rejected_total`, and `board_stratum_reconnects_total`
/// to the metrics exposition channel, one labeled series per pool
/// slot below `pool_slots`.
///
/// `pool_slots` is clamped to [`POOL_INDEX_SPACE`], the capacity of
/// the backing per-slot counter arrays; callers pass their board's
/// `POOL_SLOTS` constant.
pub async fn run_stratum_metrics_publisher(pool_slots: usize) -> ! {
    let mut ticker = Ticker::every(Duration::from_secs(2));
    loop {
        ticker.next().await;
        for slot in 0..pool_slots.min(POOL_INDEX_SPACE) {
            let Some(metrics) = crate::metrics::for_slot(slot) else {
                continue;
            };
            let snapshot = metrics.snapshot();
            push_slot_metric(
                "board_shares_accepted_total",
                slot,
                Value::Uint(snapshot.shares_accepted),
            )
            .await;
            push_slot_metric(
                "board_shares_rejected_total",
                slot,
                Value::Uint(snapshot.shares_rejected),
            )
            .await;
            push_slot_metric(
                "board_stratum_reconnects_total",
                slot,
                Value::Uint(u64::from(snapshot.reconnects)),
            )
            .await;
        }
    }
}
