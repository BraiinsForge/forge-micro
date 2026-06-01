//! Per-board metric publishers for ubos-mining state.
//!
//! Lives here rather than each firmware's `main.rs` so the
//! atomics, metric names, and cadence stay in one place. Boards
//! spawn `run_miner_metrics_publisher` from their own
//! `#[embassy_executor::task]` wrapper.

use embassy_time::{Duration, Ticker};
use portable_atomic::Ordering;
use ubos_metrics::metrics::{Metric, Value, push};

use crate::Handle as MiningHandle;
use crate::bm1362::UART_DESYNC_BYTES;

/// Periodically push `board_uart_desync_bytes_total` and
/// `board_miner_running` to the metrics exposition channel.
pub async fn run_miner_metrics_publisher(mining: MiningHandle) -> ! {
    let mut ticker = Ticker::every(Duration::from_secs(2));
    loop {
        ticker.next().await;
        let desync = u64::from(UART_DESYNC_BYTES.load(Ordering::Relaxed));
        push(Metric::with_no_labels(
            "board_uart_desync_bytes_total",
            Value::Uint(desync),
        ))
        .await;
        let running = u64::from(mining.state().is_some_and(|state| state.running));
        push(Metric::with_no_labels(
            "board_miner_running",
            Value::Uint(running),
        ))
        .await;
    }
}
