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
use ubos_metrics::catalog::{HASHBOARD_LABEL, POOL_LABEL, TYPE_LABEL};
use ubos_metrics::metric;
use ubos_metrics::value::{Count, Flag};

use crate::{Handle as MiningHandle, MiningState};
use ubos_metrics::chain as stats;

/// Publish the miner state and the confirmed accounting of each hashboard
/// every two seconds. A hashboard series carries the `hashboard` label,
/// and the pool work shares also carry the `pool` label.
/// The PSU publishes its own commanded and measured rail voltages.
pub async fn run_miner_metrics_publisher(mining: MiningHandle, pool_slots: usize) -> ! {
    let mut ticker = Ticker::every(Duration::from_secs(2));
    loop {
        ticker.next().await;
        let stats_handle = stats::handle();
        for (chain, stats) in stats_handle.chains().enumerate() {
            metric!("frequency_hertz", stats.frequency())
                .label(HASHBOARD_LABEL, chain)
                .push()
                .await;
            metric!("uart_desync_bytes", Count(u64::from(stats.desync_bytes())))
                .label(HASHBOARD_LABEL, chain)
                .push()
                .await;
            metric!("hashboard_present", Flag(stats.presence().is_present()))
                .label(HASHBOARD_LABEL, chain)
                .push()
                .await;
            metric!("hashrate_hps", Count(stats.hashrate()))
                .label(HASHBOARD_LABEL, chain)
                .push()
                .await;
            // The chain counts only the shares it found, so `valid` is the
            // one type of this series.
            metric!("hashboard_shares", stats.share_count())
                .label(HASHBOARD_LABEL, chain)
                .label(TYPE_LABEL, "valid")
                .push()
                .await;
            let limit = pool_slots.min(crate::consts::MAX_POOL_SLOTS);
            for slot in 0..limit {
                metric!("pool_work_shares", stats.pool_shares(slot))
                    .label(HASHBOARD_LABEL, chain)
                    .label(POOL_LABEL, slot)
                    .push()
                    .await;
            }
        }
        let state = mining.state();
        // One series per state, 1 on the current one, so a panel can map
        // the sum to the phase. Every series is 0 before the first publish.
        for candidate in MiningState::ALL {
            metric!("miner_state", Flag(state == Some(candidate)))
                .label("state", candidate.as_api_str())
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
