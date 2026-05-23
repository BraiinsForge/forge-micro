//! Per-board metric publishers for ubos-miner state.
//!
//! Lives here rather than each firmware's `main.rs` so the
//! atomics, metric names, and cadence stay in one place. Boards
//! spawn `run_miner_metrics_publisher` from their own
//! `#[embassy_executor::task]` wrapper.

use embassy_time::{Duration, Ticker};
use portable_atomic::Ordering;
use ubos_metrics::metrics::{Metric, Value, push};

use crate::bm1362::UART_DESYNC_BYTES;
use crate::miner::MINER_IS_RUNNING_STATE;

/// Periodically push `board_uart_desync_bytes_total` and
/// `board_miner_running` to the metrics exposition channel.
pub async fn run_miner_metrics_publisher() -> ! {
    let mut ticker = Ticker::every(Duration::from_secs(2));
    loop {
        ticker.next().await;
        let desync = u64::from(UART_DESYNC_BYTES.load(Ordering::Relaxed));
        push(Metric {
            name: "board_uart_desync_bytes_total",
            labels: heapless::Vec::new(),
            value: Value::Uint(desync),
            timestamp_ms: 0,
        })
        .await;
        let running = u64::from(MINER_IS_RUNNING_STATE.load(Ordering::Acquire));
        push(Metric {
            name: "board_miner_running",
            labels: heapless::Vec::new(),
            value: Value::Uint(running),
            timestamp_ms: 0,
        })
        .await;
    }
}
