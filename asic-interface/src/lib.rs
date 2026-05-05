//! Trait surface between a stratum-side worker and any ASIC backend.
//!
//! The worker hands the ASIC a `WorkFrame` and waits for a `RawShare`.
//! Implementations live with the backend they wrap: `cpu-miner` for the
//! in-process CPU miner, `eth-examples::uart_asic` for a UART-attached
//! external ASIC. Keeping the trait in its own crate lets either side
//! evolve without dragging the Stratum client or the SHA-256 inner loop
//! behind it.
//!
//! Hashrate accounting also lives here. [`record_share`] bumps
//! [`SHARE_COUNT`] and feeds the current share's difficulty into an
//! EWMA-smoothed bucketed estimator; [`hashrate`] reads the smoothed
//! value. The [`AsicInterface::next_share`] default method calls
//! [`record_share`] for every share returned by the backend, so any
//! `recv_share` impl gets metrics for free.

#![no_std]

use core::sync::atomic::Ordering;
use embassy_time::Instant;
use portable_atomic::{AtomicBool, AtomicU32, AtomicU64};

#[cfg(test)]
#[path = "tests/hashrate.rs"]
mod tests;

/// Header and target pair pushed to an ASIC for one mining round.
#[derive(Clone, Copy)]
pub struct WorkFrame {
    /// Serialized 80-byte block header with the starting nonce pre-stamped
    /// into bytes 76–79 by the worker.
    pub header: [u8; 80],
    /// Pool difficulty target in 256-bit little-endian form.
    pub target: [u8; 32],
    /// Worker-assigned identifier for this stratum job. Bumped on every
    /// job change so a backend that decouples mining from share consumption
    /// (channel- or UART-based) can tag the shares it produces; the worker
    /// drops shares whose `work_id` does not match the in-flight job.
    pub work_id: u32,
}

/// Share returned by an ASIC. The worker retains all header fields; the
/// ASIC only needs to report the winning nonce.
#[derive(Clone, Copy)]
pub struct RawShare {
    /// Winning nonce value in host byte order.
    pub nonce: u32,
    /// `work_id` of the `WorkFrame` this share was produced for. Lets the
    /// worker discard a share that was already in flight when a fresh job
    /// landed and would otherwise be packaged with the wrong stratum
    /// `job_id`/`extranonce2`/`ntime`.
    pub work_id: u32,
}

/// Pluggable mining backend.
// async_fn_in_trait: implementations run on a cooperative single-executor;
// Send bounds on the returned futures are neither needed nor possible
// in no_std.
#[allow(async_fn_in_trait)]
pub trait AsicInterface {
    /// Error type surfaced by send/receive operations. `Debug` is the
    /// portable bound; firmware paths wrap with `Debug2Format` for
    /// defmt printing, host paths print via `{:?}` directly.
    type Error: core::fmt::Debug;

    /// Push a new work frame to the backend. The call returns once the
    /// frame has been accepted; it does not wait for a share.
    async fn send_work(&mut self, work: &WorkFrame) -> Result<(), Self::Error>;

    /// Block until the backend produces a share for the most recently
    /// submitted work frame. Backends implement this; the stratum
    /// worker calls [`AsicInterface::next_share`] which also feeds the
    /// hashrate estimator.
    async fn recv_share(&mut self) -> Result<RawShare, Self::Error>;

    /// Block until the backend produces a share, then credit it
    /// against `wire_target` for hashrate accounting. `wire_target`
    /// must be the target the asic was scanning to. It can be easier
    /// than the pool target if the worker swaps in a denser target
    /// on the wire. It sets hashes-per-share for the EWMA bucket.
    ///
    /// Default impl wraps `recv_share`, so every backend gets share
    /// counting without having to opt in. Override only if a backend
    /// needs to short-circuit the metric path.
    async fn next_share(&mut self, wire_target: &[u8; 32]) -> Result<RawShare, Self::Error> {
        let share = self.recv_share().await?;
        record_share(wire_target);
        Ok(share)
    }
}

/// Total shares observed by [`record_share`] since boot. Monotonic;
/// rendered as inline text on the firmware HTTP status page.
pub static SHARE_COUNT: AtomicU32 = AtomicU32::new(0);

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

/// Smoothed hashrate in H/s. Updated when a bucket's worth of hashes
/// has accumulated; read by [`hashrate`].
static EWMA_HASHRATE: AtomicU32 = AtomicU32::new(0);

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
    let previous = EWMA_HASHRATE.load(Ordering::Relaxed);
    EWMA_HASHRATE.store(
        next_ewma(bucket_total, elapsed_ms, previous),
        Ordering::Relaxed,
    );
}

/// Fold a finalized bucket into the EWMA. Pure: takes the bucket's
/// total hashes, the elapsed window in ms, and the previous EWMA, and
/// returns the new EWMA. Saturates at `u32::MAX` when the bucket rate
/// or the smoothed value would not fit in a `u32`.
fn next_ewma(bucket_total: u64, elapsed_ms: u64, previous: u32) -> u32 {
    let bucket_rate =
        u32::try_from(bucket_total.saturating_mul(1000) / elapsed_ms).unwrap_or(u32::MAX);
    let delta = i64::from(bucket_rate) - i64::from(previous);
    let next = i64::from(previous) + (delta >> EWMA_ALPHA_SHIFT);
    u32::try_from(next.max(0)).unwrap_or(u32::MAX)
}

/// Smoothed hashrate in hashes per second. Zero until the first
/// bucket finalizes, `BUCKET_MS` after the first share.
#[must_use]
pub fn hashrate() -> u32 {
    EWMA_HASHRATE.load(Ordering::Relaxed)
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
