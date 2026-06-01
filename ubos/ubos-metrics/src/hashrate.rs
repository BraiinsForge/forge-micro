//! Bucketed EWMA hashrate accounting shared by every firmware.
//!
//! Metrics: `../../../docs/METRICS.md`.

use embassy_time::Instant;
use portable_atomic::{AtomicBool, AtomicU64, Ordering};

#[cfg(test)]
#[path = "tests/hashrate.rs"]
mod tests;

/// Total shares observed by [`record_share`] since boot. Monotonic;
/// rendered as inline text on the firmware HTTP status page.
pub static SHARE_COUNT: AtomicU64 = AtomicU64::new(0);

/// EWMA smoothing factor exponent: shift right by this many bits to
/// scale the per-bucket delta. 3 → α = 1/8 → roughly an 8-bucket
/// window, i.e. ~8 s with [`BUCKET_MS`].
const EWMA_ALPHA_SHIFT: u32 = 3;

/// Width of the rate-accumulation bucket. Computing instant rate as
/// `hashes_per_share / Δt_per_share` is biased high on a Poisson
/// process, so [`record_share`] sums hashes over `BUCKET_MS` and
/// only feeds the resulting average rate into the EWMA. With 1 s
/// buckets and α = 1/8 the smoothing window is ~8 s.
const BUCKET_MS: u64 = 1_000;

/// Beyond this many missed buckets the read-side decay in [`hashrate`]
/// clamps to zero. 64 steps of α = 1/8 reduce any starting value to
/// less than 0.02% of the original, which rounds to zero in u64.
const MAX_DECAY_STEPS: u64 = 64;

/// Smoothed hashrate in H/s. Updated when a bucket's worth of hashes
/// has accumulated; read by [`hashrate`].
static EWMA_HASHRATE: AtomicU64 = AtomicU64::new(0);

/// Hashes credited to the in-progress bucket. Drained when the bucket
/// elapses, see [`record_share`].
static BUCKET_HASHES: AtomicU64 = AtomicU64::new(0);

/// Uptime ms at which the in-progress bucket started. Only valid
/// once [`BUCKET_PRIMED`] is true.
static BUCKET_START_MS: AtomicU64 = AtomicU64::new(0);

/// False until the first share has been recorded; gates the
/// "first share primes the bucket" branch in [`record_share`] so the
/// branch is not retriggered if [`Instant::now`] happens to read 0 ms
/// at the moment the first share lands.
static BUCKET_PRIMED: AtomicBool = AtomicBool::new(false);

/// Bump the share counter and credit this share's hashes toward the
/// current bucket. When the bucket has run for at least `BUCKET_MS`,
/// finalize it: compute the average rate over the elapsed window and
/// fold that into the EWMA. The first share just primes the bucket.
///
/// Single-writer so the load/store sequences on `BUCKET_*` and
/// `EWMA_HASHRATE` cannot interleave with themselves; readers of
/// [`hashrate`] only see the post-store value.
pub fn record_share(target: &[u8; 32]) {
    SHARE_COUNT.fetch_add(1, Ordering::Relaxed);

    let hashes = hashes_per_share(target);
    let now_ms = Instant::now().as_millis();

    // First share after boot: prime the bucket and return; we need a
    // second share before there is any elapsed time to compute over.
    if !BUCKET_PRIMED.swap(true, Ordering::Relaxed) {
        BUCKET_START_MS.store(now_ms, Ordering::Relaxed);
        BUCKET_HASHES.store(hashes, Ordering::Relaxed);
        return;
    }

    let bucket_start = BUCKET_START_MS.load(Ordering::Relaxed);
    let elapsed_ms = now_ms.saturating_sub(bucket_start);
    if elapsed_ms < BUCKET_MS {
        BUCKET_HASHES.fetch_add(hashes, Ordering::Relaxed);
        return;
    }

    // Finalize the bucket. Swap so concurrent readers still see a
    // coherent snapshot.
    let bucket_total = BUCKET_HASHES.swap(hashes, Ordering::Relaxed);
    BUCKET_START_MS.store(now_ms, Ordering::Relaxed);
    let stored = EWMA_HASHRATE.load(Ordering::Relaxed);
    // When a share arrives after silence, decay the stored EWMA across
    // the missed buckets first so the resumed value is consistent with
    // what hashrate() was returning during the quiet period.
    let (previous, fold_window) = if elapsed_ms >= 2 * BUCKET_MS {
        let silent_buckets = (elapsed_ms - BUCKET_MS) / BUCKET_MS;
        (decay_silent(stored, silent_buckets), BUCKET_MS)
    } else {
        (stored, elapsed_ms)
    };
    EWMA_HASHRATE.store(
        next_ewma(bucket_total, fold_window, previous),
        Ordering::Relaxed,
    );
}

/// Apply `missed` consecutive zero-rate EWMA steps to `value`. Returns 0
/// once `missed >= MAX_DECAY_STEPS` because at α = 1/8 the residual after
/// 64 steps is below 0.02% of the original.
fn decay_silent(value: u64, missed: u64) -> u64 {
    if missed >= MAX_DECAY_STEPS {
        return 0;
    }
    let mut decayed = value;
    for _ in 0..missed {
        decayed = next_ewma(0, BUCKET_MS, decayed);
        if decayed == 0 {
            break;
        }
    }
    decayed
}

/// Fold a finalized bucket into the EWMA. Pure: takes the bucket's
/// total hashes, the elapsed window in ms, and the previous EWMA, and
/// returns the new EWMA. The bucket rate saturates at `u64::MAX`
/// through `saturating_mul`; the i128 delta arithmetic carries the
/// signed difference between two u64 values without overflow.
fn next_ewma(bucket_total: u64, elapsed_ms: u64, previous: u64) -> u64 {
    let bucket_rate = bucket_total.saturating_mul(1000) / elapsed_ms;
    let delta = i128::from(bucket_rate) - i128::from(previous);
    let next = i128::from(previous) + (delta >> EWMA_ALPHA_SHIFT);
    u64::try_from(next.max(0)).unwrap_or(u64::MAX)
}

/// Smoothed hashrate in hashes per second. Zero until the first
/// bucket finalizes, `BUCKET_MS` after the first share.
///
/// When shares stop arriving the stored EWMA goes stale because
/// only [`record_share`] writes it. This function applies read-side
/// decay: for every full `BUCKET_MS` window of staleness beyond the
/// first two, it folds a zero-rate bucket into the EWMA. The result
/// is never written back, preserving the single-writer invariant.
#[must_use]
pub fn hashrate() -> u64 {
    let stored = EWMA_HASHRATE.load(Ordering::Relaxed);
    if stored == 0 || !BUCKET_PRIMED.load(Ordering::Relaxed) {
        return stored;
    }

    let bucket_start = BUCKET_START_MS.load(Ordering::Relaxed);
    let now_ms = Instant::now().as_millis();
    let stale_ms = now_ms.saturating_sub(bucket_start);

    // The current bucket is still accumulating or just barely overdue.
    if stale_ms < 2 * BUCKET_MS {
        return stored;
    }

    let missed = (stale_ms - BUCKET_MS) / BUCKET_MS;
    decay_silent(stored, missed)
}

/// Periodically push `board_hashrate_hps` and `board_shares_total` to
/// the metrics exposition channel.
///
/// Reads the same atomics as [`hashrate`] and [`SHARE_COUNT`]. Boards
/// spawn this from their own `#[embassy_executor::task]` wrapper so the
/// pool/stack/cap configuration stays per-target.
pub async fn run_hashrate_publisher() -> ! {
    use embassy_time::{Duration, Ticker};

    use crate::metrics::{push, Metric, Value};

    let mut ticker = Ticker::every(Duration::from_secs(2));
    loop {
        ticker.next().await;
        push(Metric::with_no_labels(
            "board_hashrate_hps",
            Value::Uint(hashrate()),
        ))
        .await;
        // ASIC-reported share count, distinct from the upstream pool's
        // accepted/rejected acknowledgements (`board_shares_accepted_total`
        // and `board_shares_rejected_total` published by stratum-client).
        push(Metric::with_no_labels(
            "board_shares_total",
            Value::Uint(SHARE_COUNT.load(Ordering::Relaxed)),
        ))
        .await;
    }
}

/// `Display` adapter for a hashrate in H/s. Renders rates below 1 GH/s
/// as `"<n> H/s"` and rates at or above 1 GH/s as `"<n>.<dd> GH/s"`
/// with two fractional digits. The threshold is the same magnitude the
/// pool dashboard prefers, so a board crossing into GH/s territory
/// stops printing 10-digit integers on the OLED and HTTP page.
pub struct HashrateDisplay(pub u64);

impl core::fmt::Display for HashrateDisplay {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        const ONE_GH: u64 = 1_000_000_000;
        const HUNDREDTH_GH: u64 = ONE_GH / 100;
        if self.0 >= ONE_GH {
            let whole = self.0 / ONE_GH;
            let hundredths = (self.0 % ONE_GH) / HUNDREDTH_GH;
            write!(formatter, "{whole}.{hundredths:02} GH/s")
        } else {
            write!(formatter, "{} H/s", self.0)
        }
    }
}

/// Approximate hashes per share via leading-zero bits of the 256-bit
/// little-endian target. Coarse but bounded: within a factor of two
/// of the exact `2^256 / (target+1)` value, which is good enough for
/// a rolling-window display.
fn hashes_per_share(target: &[u8; 32]) -> u64 {
    let mut bits: u32 = 0;
    // The array is little-endian, so the most significant byte is at
    // index 31. Walk from MSB toward LSB counting leading zero bits.
    for byte in target.iter().rev() {
        if *byte == 0 {
            bits += 8;
            continue;
        }
        bits += byte.leading_zeros();
        break;
    }
    1u64.checked_shl(bits).unwrap_or(u64::MAX)
}
