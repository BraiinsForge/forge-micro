//! Per-board metric publishers for stratum-client state.
//!
//! Lives here rather than each firmware's `main.rs` so the
//! atomics, metric names, and cadence stay in one place. Boards
//! spawn `run_stratum_metrics_publisher` from their own
//! `#[embassy_executor::task]` wrapper.

use core::fmt::Write as _;

use embassy_time::{Duration, Ticker};
use portable_atomic::Ordering;
use ubos_metrics::metrics::{Label, LabelValue, Labels, Metric, Value, push};

use crate::run::{POOL_INDEX_SPACE, STRATUM_RECONNECTS};
use crate::state::{SHARES_ACCEPTED, SHARES_REJECTED};

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
            push_slot_metric(
                "board_shares_accepted_total",
                slot,
                Value::Uint(SHARES_ACCEPTED[slot].load(Ordering::Relaxed)),
            )
            .await;
            push_slot_metric(
                "board_shares_rejected_total",
                slot,
                Value::Uint(SHARES_REJECTED[slot].load(Ordering::Relaxed)),
            )
            .await;
            push_slot_metric(
                "board_stratum_reconnects_total",
                slot,
                Value::Uint(u64::from(STRATUM_RECONNECTS[slot].load(Ordering::Relaxed))),
            )
            .await;
        }
    }
}

/// Push one metric labeled `pool="<slot>"`.
async fn push_slot_metric(name: &'static str, slot: usize, value: Value) {
    let mut slot_str = heapless::String::<24>::new();
    let _ = write!(slot_str, "{slot}");
    let mut labels = Labels::new();
    let _ = labels.push(Label {
        name: "pool",
        value: LabelValue::Short(slot_str),
    });
    push(Metric::with_labels(name, labels, value)).await;
}
