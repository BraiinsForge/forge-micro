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
use ubos_metrics::metrics::{Label, LabelValue, Labels, Metric, Value, push};

use crate::{Handle as MiningHandle, MiningState};
use ubos_metrics::chain as stats;

/// Publish lifecycle status and each chain's confirmed accounting every two seconds.
/// Chain metrics carry `chain="i"`; pool shares also carry `pool="i"`,
/// or `pool="fee"` for the board's fee slot.
/// The PSU publishes its own commanded and measured rail voltages.
pub async fn run_miner_metrics_publisher(
    mining: MiningHandle,
    pool_slots: usize,
    fee_slot: Option<usize>,
) -> ! {
    let mut ticker = Ticker::every(Duration::from_secs(2));
    loop {
        ticker.next().await;
        let stats_handle = stats::handle();
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
            push_chain_metric(
                "board_chain_frequency_khz",
                chain,
                Value::Uint(u64::from(stats.frequency().khz())),
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
                ubos_metrics::metrics::push_chain_pool_metric(
                    "board_pool_work_shares_total",
                    chain,
                    slot,
                    fee_slot,
                    Value::Uint(stats.pool_shares(slot)),
                )
                .await;
            }
        }
        let state = mining.state();
        let running = u64::from(matches!(state, Some(MiningState::Running)));
        push(Metric::with_no_labels(
            "board_miner_running",
            Value::Uint(running),
        ))
        .await;
        // One series per state, 1 on the current one, so a panel can
        // show which phase the miner is in rather than only whether it
        // hashes. Every series is 0 before the first publish.
        for candidate in MiningState::ALL {
            let mut labels = Labels::new();
            let _ = labels.push(Label {
                name: "state",
                value: LabelValue::Static(candidate.as_api_str()),
            });
            push(Metric::with_labels(
                "board_miner_state",
                labels,
                Value::Uint(u64::from(state == Some(candidate))),
            ))
            .await;
        }
    }
}

/// Push one metric labeled `chain="<chain>"`.
async fn push_chain_metric(name: &'static str, chain: usize, value: Value) {
    ubos_metrics::metrics::push_indexed_metric(name, "chain", chain, value).await;
}

/// Task shell around [`run_miner_metrics_publisher`]. Takes the board's
/// pool slot count, which is a per-board const.
#[macro_export]
macro_rules! miner_metrics_task {
    ($pool_slots:expr, $fee:expr) => {
        #[embassy_executor::task]
        async fn miner_metrics_task(mining: ubos_mining::Handle) -> ! {
            ubos_mining::publishers::run_miner_metrics_publisher(
                mining,
                $pool_slots,
                ubos_config::FeeSlot::index(&$fee),
            )
            .await
        }
    };
}
