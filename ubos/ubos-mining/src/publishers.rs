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

use embassy_futures::join::join;
use embassy_time::{Duration, Ticker};
use ubos_asic::HashboardDescriptor;
use ubos_metrics::Metric;
use ubos_metrics::catalog::{HASHBOARD_LABEL, POOL_LABEL, TYPE_LABEL};
use ubos_metrics::metric;
use ubos_metrics::value::{Count, Flag};

use crate::{Handle as MiningHandle, MiningState};

#[cfg(test)]
#[path = "tests/publishers.rs"]
mod tests;

/// The chips a chain answered at its last enumeration, which is zero for
/// a chain that answered none. Grafana divides it by `chips_expected`, and
/// the verdict on the chain stays in `hashboard_present`.
pub(crate) fn chips_discovered(chain: u8, found: u8) -> Metric {
    metric!("chips_discovered", Count(u64::from(found)))
        .label(HASHBOARD_LABEL, chain)
        .constant()
}

/// Publish the counts of hashboards, chips and voltage domains the
/// machine is built with. One descriptor describes every chain, so the
/// series carry no `hashboard` label.
pub(crate) async fn publish_machine_facts(board: &HashboardDescriptor, chains: usize) {
    metric!("chips_expected", Count(u64::from(board.num_chips)))
        .constant()
        .push()
        .await;
    metric!(
        "voltage_domains",
        Count(u64::from(board.num_voltage_domains))
    )
    .constant()
    .push()
    .await;
    metric!("hashboards", Count(chains as u64))
        .constant()
        .push()
        .await;
}

/// How often the chain snapshot and the metrics are published.
const PUBLISH_INTERVAL: Duration = Duration::from_secs(2);

/// Publish the miner state and the confirmed accounting of each hashboard
/// every [`PUBLISH_INTERVAL`], to the metrics store and to the readers of
/// [`MiningHandle::chains`]. The two run as separate loops, because a
/// metric push waits while the metrics channel is full and the snapshot
/// must not wait with it.
pub async fn run_miner_metrics_publisher(mining: MiningHandle, pool_slots: usize) -> ! {
    join(publish_chains(), push_metrics(mining, pool_slots))
        .await
        .0
}

async fn publish_chains() -> ! {
    let mut ticker = Ticker::every(PUBLISH_INTERVAL);
    loop {
        ticker.next().await;
        crate::control::publish_chains();
    }
}

/// A hashboard series carries the `hashboard` label, and the pool work
/// shares also carry the `pool` label. The PSU publishes its own commanded
/// and measured rail voltages.
async fn push_metrics(mining: MiningHandle, pool_slots: usize) -> ! {
    let mut ticker = Ticker::every(PUBLISH_INTERVAL);
    loop {
        ticker.next().await;
        let chains = mining.chains();
        for (chain, stats) in chains.iter().enumerate() {
            metric!("frequency_hertz", stats.frequency)
                .label(HASHBOARD_LABEL, chain)
                .push()
                .await;
            metric!("uart_desync_bytes", Count(u64::from(stats.desync_bytes)))
                .label(HASHBOARD_LABEL, chain)
                .push()
                .await;
            metric!("hashboard_present", Flag(stats.presence.is_present()))
                .label(HASHBOARD_LABEL, chain)
                .push()
                .await;
            metric!("hashrate_hps", stats.hashrate)
                .label(HASHBOARD_LABEL, chain)
                .push()
                .await;
            // The chain counts only the shares it found, so `valid` is the
            // one type of this series.
            metric!("hashboard_shares", stats.shares)
                .label(HASHBOARD_LABEL, chain)
                .label(TYPE_LABEL, "valid")
                .push()
                .await;
            let limit = pool_slots.min(crate::consts::MAX_POOL_SLOTS);
            for (slot, shares) in stats.pool_shares.iter().take(limit).enumerate() {
                metric!("pool_work_shares", *shares)
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
