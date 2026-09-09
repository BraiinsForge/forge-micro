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

//! Per-board metric publishers for ubos-mining state.
//!
//! Lives here rather than each firmware's `main.rs` so the
//! atomics, metric names, and cadence stay in one place. Boards
//! spawn `run_miner_metrics_publisher` from their own
//! `#[embassy_executor::task]` wrapper.

use embassy_time::{Duration, Ticker};
use ubos_metrics::metrics::{Metric, Value, push};

use crate::Handle as MiningHandle;
use crate::chain_stats;

/// Periodically push `board_uart_desync_bytes_total`,
/// `board_chain_chips`, `board_chain_present`, `board_hashrate_hps`,
/// `board_shares_total`, `board_miner_running`,
/// `board_miner_frequency_khz`, `board_miner_voltage_mv`, and
/// `board_pool_work_shares_total` to the metrics exposition channel.
/// The frequency and voltage gauges track the tuner's live operating
/// point — they update on every ramp step, not only at the configured
/// target. The frequency gauge is exposed in kHz so the 6.25 MHz
/// `FixedTuner` ramp granularity survives the integer cast.
/// `board_uart_desync_bytes_total`, `board_chain_chips`,
/// `board_chain_present`, `board_hashrate_hps`, and
/// `board_shares_total` are each labeled `chain="i"` and pushed once
/// per chain the machine wired up, which
/// [`chain_stats::ChainStatsHandle::chain_count`] reports, so the unused
/// slots of the table are never published at all. That count is zero
/// until the miner starts, and until then no per-chain series is
/// emitted. No unlabelled duplicate of the hashrate and share series is
/// emitted; a machine total is the sum over the `chain` label.
/// `board_pool_work_shares_total` is
/// additionally labeled `pool="i"` and pushed once per pool slot below
/// `pool_slots`, which callers pass as their board's `POOL_SLOTS`
/// constant.
pub async fn run_miner_metrics_publisher(mining: MiningHandle, pool_slots: usize) -> ! {
    let mut ticker = Ticker::every(Duration::from_secs(2));
    loop {
        ticker.next().await;
        let stats_handle = chain_stats::handle();
        for (chain, stats) in stats_handle.chains().enumerate() {
            push_chain_metric(
                "board_uart_desync_bytes_total",
                chain,
                Value::Uint(u64::from(stats.desync_bytes())),
            )
            .await;
            push_chain_metric(
                "board_chain_chips",
                chain,
                Value::Uint(u64::from(stats.chips())),
            )
            .await;
            push_chain_metric(
                "board_chain_present",
                chain,
                Value::Uint(u64::from(stats.presence().is_present())),
            )
            .await;
            push_chain_metric("board_hashrate_hps", chain, Value::Uint(stats.hashrate())).await;
            push_chain_metric(
                "board_shares_total",
                chain,
                Value::Uint(stats.share_count()),
            )
            .await;
            let limit = pool_slots.min(crate::consts::MAX_POOL_SLOTS);
            for slot in 0..limit {
                push_chain_pool_metric(
                    "board_pool_work_shares_total",
                    chain,
                    slot,
                    Value::Uint(stats.pool_shares(slot)),
                )
                .await;
            }
        }
        let state = mining.state();
        let running = u64::from(state.is_some_and(|state| state.running));
        push(Metric::with_no_labels(
            "board_miner_running",
            Value::Uint(running),
        ))
        .await;
        let frequency_khz = u64::from(state.map_or(0, |state| state.frequency.khz()));
        push(Metric::with_no_labels(
            "board_miner_frequency_khz",
            Value::Uint(frequency_khz),
        ))
        .await;
        let voltage_mv = u64::from(state.map_or(0, |state| state.voltage.mv()));
        push(Metric::with_no_labels(
            "board_miner_voltage_mv",
            Value::Uint(voltage_mv),
        ))
        .await;
    }
}

/// Push one metric labeled `chain="<chain>"`.
async fn push_chain_metric(name: &'static str, chain: usize, value: Value) {
    ubos_metrics::metrics::push_indexed_metric(name, "chain", chain, value).await;
}

/// Push one metric labeled `chain="<chain>"` and `pool="<slot>"`.
async fn push_chain_pool_metric(name: &'static str, chain: usize, slot: usize, value: Value) {
    ubos_metrics::metrics::push_indexed_metrics(name, &[("chain", chain), ("pool", slot)], value)
        .await;
}

/// Task shell around [`run_miner_metrics_publisher`]. Takes the board's
/// pool slot count, which is a per-board const.
#[macro_export]
macro_rules! miner_metrics_task {
    ($pool_slots:expr) => {
        #[embassy_executor::task]
        async fn miner_metrics_task(mining: ubos_mining::Handle) -> ! {
            ubos_mining::publishers::run_miner_metrics_publisher(mining, $pool_slots).await
        }
    };
}
