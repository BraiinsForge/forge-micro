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
use ubos_metrics::metrics::{Label, LabelValue, Labels, Metric};
use ubos_metrics::value::Value;

use crate::{Handle as MiningHandle, MiningState};
use ubos_metrics::chain as stats;

/// Publish lifecycle status and each chain's confirmed accounting every two seconds.
/// Chain metrics carry `chain="i"`; pool shares also carry `pool="i"`.
/// The PSU publishes its own commanded and measured rail voltages.
pub async fn run_miner_metrics_publisher(mining: MiningHandle, pool_slots: usize) -> ! {
    let mut ticker = Ticker::every(Duration::from_secs(2));
    loop {
        ticker.next().await;
        let stats_handle = stats::handle();
        for (chain, stats) in stats_handle.chains().enumerate() {
            let by_chain = [("chain", chain)];
            Metric::indexed("board_chain_frequency_hertz", &by_chain, stats.frequency())
                .push()
                .await;
            let values = [
                (
                    "board_uart_desync_bytes_total",
                    Value::Count(u64::from(stats.desync_bytes())),
                ),
                ("board_chain_chips", Value::Count(u64::from(stats.chips()))),
                (
                    "board_chain_present",
                    Value::Flag(stats.presence().is_present()),
                ),
                ("board_hashrate_hps", Value::Count(stats.hashrate())),
                ("board_shares_total", Value::Count(stats.share_count())),
            ];
            for (name, value) in values {
                Metric::indexed(name, &by_chain, value).push().await;
            }
            let limit = pool_slots.min(crate::consts::MAX_POOL_SLOTS);
            for slot in 0..limit {
                Metric::indexed(
                    "board_pool_work_shares_total",
                    &[("chain", chain), ("pool", slot)],
                    Value::Count(stats.pool_shares(slot)),
                )
                .push()
                .await;
            }
        }
        let state = mining.state();
        let running = Value::Flag(matches!(state, Some(MiningState::Running)));
        Metric::with_no_labels("board_miner_running", running)
            .push()
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
            Metric::with_labels(
                "board_miner_state",
                labels,
                Value::Flag(state == Some(candidate)),
            )
            .push()
            .await;
        }
    }
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
