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
use portable_atomic::Ordering;
use ubos_metrics::metrics::{Metric, Value, push, push_slot_metric};

use crate::Handle as MiningHandle;
use crate::serial::{POOL_WORK_SHARES, UART_DESYNC_BYTES};

/// Periodically push `board_uart_desync_bytes_total`,
/// `board_miner_running`, `board_miner_frequency_khz`,
/// `board_miner_voltage_mv`, and `board_pool_work_shares_total` to
/// the metrics exposition channel. The frequency and voltage gauges
/// track the tuner's live operating point — they update on every
/// ramp step, not only at the configured target. The frequency gauge
/// is exposed in kHz so the 6.25 MHz `FixedTuner` ramp granularity
/// survives the integer cast. `board_pool_work_shares_total` is
/// pushed once per pool slot below `pool_slots`, labeled `pool="i"`;
/// `pool_slots` is clamped to [`POOL_WORK_SHARES`]'s capacity, so
/// callers can pass their board's `POOL_SLOTS` constant directly.
pub async fn run_miner_metrics_publisher(mining: MiningHandle, pool_slots: usize) -> ! {
    let mut ticker = Ticker::every(Duration::from_secs(2));
    loop {
        ticker.next().await;
        let desync = u64::from(UART_DESYNC_BYTES.load(Ordering::Relaxed));
        push(Metric::with_no_labels(
            "board_uart_desync_bytes_total",
            Value::Uint(desync),
        ))
        .await;
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
        let limit = pool_slots.min(POOL_WORK_SHARES.len());
        for (slot, counter) in POOL_WORK_SHARES.iter().enumerate().take(limit) {
            push_slot_metric(
                "board_pool_work_shares_total",
                slot,
                Value::Uint(counter.load(Ordering::Relaxed)),
            )
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
