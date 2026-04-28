//! Share counter and on-demand hashrate.
//!
//! Two metrics exposed: [`SHARE_COUNT`], a monotonic counter that the
//! mining loop bumps for every accepted Share, and [`hashrate`], which
//! computes a rate over the most recent completed window.  No
//! background task — the hashrate function maintains its own state and
//! advances the window lazily on each call.
//!
//! Calls within a [`HASHRATE_WINDOW_MS`] window return the cached rate
//! from the previous window so display and HTTP polls running faster
//! than the window do not jitter.  Once the window elapses the next
//! caller computes a new rate from the share-count delta and shifts the
//! window forward.

use embassy_time::Instant;
use portable_atomic::{AtomicU32, AtomicU64, Ordering};

#[cfg(test)]
#[path = "tests/hashrate.rs"]
mod tests;

/// Total Share frames accepted since boot.  Monotonic; expose this on
/// the metrics surface alongside the derived hashrate so consumers can
/// reconstruct any custom window they need.
pub static SHARE_COUNT: AtomicU32 = AtomicU32::new(0);

/// Window length for the hashrate estimate.  Thirty seconds is long
/// enough to absorb short-term jitter in the UART-bounded share rate
/// while still tracking real changes within a few windows.  Trades
/// reactivity for stability; raise for more averaging, lower for
/// faster response.
pub const HASHRATE_WINDOW_MS: u64 = 30_000;

/// Uptime in milliseconds at which the current window started.
static WINDOW_START_MS: AtomicU64 = AtomicU64::new(0);
/// Share count at the start of the current window.
static WINDOW_START_COUNT: AtomicU32 = AtomicU32::new(0);
/// Rate over the last completed window, in hashes per second.  Cached
/// here so calls within a single window all return the same value.
static LATEST_RATE_HZ: AtomicU32 = AtomicU32::new(0);

/// Bump the share counter.  Called from every code path that
/// successfully validates a Share frame from the dummy-asic peer.
pub fn record_share() {
    SHARE_COUNT.fetch_add(1, Ordering::Relaxed);
}

/// Override the cached rate from a miner that produces a precomputed
/// Hz value rather than discrete shares.  The cpu-mining feature on
/// the firmware crates uses this path.  Resets the window so the
/// recompute path does not zero the value before the next external
/// write.  Callers must invoke this more often than
/// [`HASHRATE_WINDOW_MS`] to keep the reading fresh.
pub fn record_external_rate(rate_hz: u32) {
    LATEST_RATE_HZ.store(rate_hz, Ordering::Relaxed);
    WINDOW_START_MS.store(Instant::now().as_millis(), Ordering::Relaxed);
    WINDOW_START_COUNT.store(SHARE_COUNT.load(Ordering::Relaxed), Ordering::Relaxed);
}

/// Hashrate over the most recent completed window, in hashes per
/// second.  Zero until the first window has elapsed.
///
/// `target_leading_zero_bytes` is the difficulty target the solver is
/// running at; it determines the expected hashes per share via
/// `2^(8 * target_leading_zero_bytes)`.
///
/// Multiple concurrent callers can race the window-advance step.  The
/// race is benign: each computes a similar rate over a slightly
/// different window and the last writer wins.  No locking required at
/// this granularity.
pub fn hashrate(target_leading_zero_bytes: u8) -> u32 {
    let now_ms = Instant::now().as_millis();
    let window_start_ms = WINDOW_START_MS.load(Ordering::Relaxed);
    let elapsed_ms = now_ms.saturating_sub(window_start_ms);

    if elapsed_ms < HASHRATE_WINDOW_MS {
        return LATEST_RATE_HZ.load(Ordering::Relaxed);
    }

    let now_count = SHARE_COUNT.load(Ordering::Relaxed);
    let prev_count = WINDOW_START_COUNT.load(Ordering::Relaxed);
    // SHARE_COUNT is u32 and wraps after 4 G shares.  The next sample
    // window resets WINDOW_START_COUNT to now_count, so a wrap shows up
    // as a single near-zero rate sample and self-corrects.
    let delta = u64::from(now_count.wrapping_sub(prev_count));

    // No new shares — the share-count path produced no new measurement.
    // Advance the window without overwriting LATEST_RATE_HZ so the
    // record_external_rate writer keeps its value visible.
    if delta == 0 {
        WINDOW_START_MS.store(now_ms, Ordering::Relaxed);
        return LATEST_RATE_HZ.load(Ordering::Relaxed);
    }

    let rate = compute_rate(delta, elapsed_ms, target_leading_zero_bytes);

    WINDOW_START_MS.store(now_ms, Ordering::Relaxed);
    WINDOW_START_COUNT.store(now_count, Ordering::Relaxed);
    LATEST_RATE_HZ.store(rate, Ordering::Relaxed);
    rate
}

/// Convert a share-count delta over an elapsed window into a hashrate
/// in H/s.  Assumes `delta > 0` and `elapsed_ms > 0`; the public
/// [`hashrate`] entry point short-circuits before calling.
fn compute_rate(delta: u64, elapsed_ms: u64, target_leading_zero_bytes: u8) -> u32 {
    // At 8+ leading zero bytes the per-share hash count reaches 2^64
    // and overflows u64; saturate to u64::MAX so the downstream
    // saturating_mul absorbs the ambiguity rather than panicking in
    // debug or wrapping in release.
    let hashes_per_share = 1u64
        .checked_shl(8 * u32::from(target_leading_zero_bytes))
        .unwrap_or(u64::MAX);
    let total = delta.saturating_mul(hashes_per_share);
    u32::try_from(total.saturating_mul(1000) / elapsed_ms).unwrap_or(u32::MAX)
}
