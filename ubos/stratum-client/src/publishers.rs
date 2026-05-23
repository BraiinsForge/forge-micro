//! Per-board metric publishers for stratum-client state.
//!
//! Lives here rather than each firmware's `main.rs` so the
//! atomics, metric names, and cadence stay in one place. Boards
//! spawn `run_stratum_metrics_publisher` from their own
//! `#[embassy_executor::task]` wrapper.

use embassy_time::{Duration, Ticker};
use portable_atomic::Ordering;
use ubos_metrics::metrics::{push, Metric, Value};

use crate::run::STRATUM_RECONNECTS;
use crate::state::{SHARES_ACCEPTED, SHARES_REJECTED};

/// Periodically push `board_shares_accepted_total`,
/// `board_shares_rejected_total`, and `board_stratum_reconnects_total`
/// to the metrics exposition channel.
pub async fn run_stratum_metrics_publisher() -> ! {
    let mut ticker = Ticker::every(Duration::from_secs(2));
    loop {
        ticker.next().await;
        push(Metric {
            name: "board_shares_accepted_total",
            labels: heapless::Vec::new(),
            value: Value::Uint(SHARES_ACCEPTED.load(Ordering::Relaxed)),
            timestamp_ms: 0,
        })
        .await;
        push(Metric {
            name: "board_shares_rejected_total",
            labels: heapless::Vec::new(),
            value: Value::Uint(SHARES_REJECTED.load(Ordering::Relaxed)),
            timestamp_ms: 0,
        })
        .await;
        push(Metric {
            name: "board_stratum_reconnects_total",
            labels: heapless::Vec::new(),
            value: Value::Uint(u64::from(STRATUM_RECONNECTS.load(Ordering::Relaxed))),
            timestamp_ms: 0,
        })
        .await;
    }
}
