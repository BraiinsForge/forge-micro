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

//! Bucketed EWMA hashrate accounting, instantiated once per estimator.
//!
//! Metrics: `../../../docs/METRICS.md`.

use embassy_time::Instant;
use portable_atomic::{AtomicBool, AtomicU64, Ordering};

#[cfg(test)]
#[path = "tests/hashrate.rs"]
mod tests;

/// EWMA smoothing factor exponent: shift right by this many bits to
/// scale the per-bucket delta. 3 → α = 1/8 → roughly an 8-bucket
/// window, i.e. ~8 s with [`BUCKET_MS`].
const EWMA_ALPHA_SHIFT: u32 = 3;

/// Width of the rate-accumulation bucket. Computing instant rate as
/// `hashes_per_share / Δt_per_share` is biased high on a Poisson
/// process, so [`HashrateEstimator::record_share`] sums hashes over
/// `BUCKET_MS` and only feeds the resulting average rate into the
/// EWMA. With 1 s buckets and α = 1/8 the smoothing window is ~8 s.
const BUCKET_MS: u64 = 1_000;

/// Beyond this many missed buckets the read-side decay in
/// [`HashrateEstimator::hashrate`] clamps to zero. 64 steps of α = 1/8
/// reduce any starting value to less than 0.02% of the original, which
/// rounds to zero in u64.
const MAX_DECAY_STEPS: u64 = 64;

/// Expected hashes behind one share, the unit every share counter
/// reports in. A chip solution found against a difficulty `d` target
/// counts as `d` shares, so a counter times this constant is the
/// expected hash count whatever filter the chip runs.
pub const HASHES_PER_SHARE: u64 = 1 << 32;

/// A single instance's bucketed EWMA hashrate accounting.
///
/// One instance per chain, expected to be written by exactly one
/// actor calling [`record_share`](Self::record_share); readers of
/// [`hashrate`](Self::hashrate) only see the post-store value, and the
/// load/store sequences on the fields below cannot interleave with
/// themselves as long as that single-writer invariant holds.
pub struct HashrateEstimator {
    /// Total shares, in units of [`HASHES_PER_SHARE`], credited by
    /// [`record_share`](Self::record_share) since this instance was
    /// created.
    share_count: AtomicU64,
    /// Smoothed hashrate in H/s. Updated when a bucket's worth of
    /// hashes has accumulated; read by [`hashrate`](Self::hashrate).
    ewma: AtomicU64,
    /// Hashes credited to the in-progress bucket. Drained when the
    /// bucket elapses, see [`record_share`](Self::record_share).
    bucket_hashes: AtomicU64,
    /// Uptime ms at which the in-progress bucket started. Only valid
    /// once [`bucket_primed`](Self::bucket_primed) is true.
    bucket_start_ms: AtomicU64,
    /// False until the first share has been recorded; gates the
    /// "first share primes the bucket" branch in
    /// [`record_share`](Self::record_share) so the branch is not
    /// retriggered if [`Instant::now`] happens to read 0 ms at the
    /// moment the first share lands.
    bucket_primed: AtomicBool,
}

impl HashrateEstimator {
    /// Construct an estimator with no shares recorded yet.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            share_count: AtomicU64::new(0),
            ewma: AtomicU64::new(0),
            bucket_hashes: AtomicU64::new(0),
            bucket_start_ms: AtomicU64::new(0),
            bucket_primed: AtomicBool::new(false),
        }
    }

    /// Total shares, in units of [`HASHES_PER_SHARE`], credited since
    /// this instance was created.
    #[must_use]
    pub fn share_count(&self) -> u64 {
        self.share_count.load(Ordering::Relaxed)
    }

    /// Credit one chip solution found against `target`: its share
    /// weight, the target's difficulty in units of [`HASHES_PER_SHARE`],
    /// goes to the share counter and its hashes toward the current
    /// bucket. When the bucket has run for at least `BUCKET_MS`,
    /// finalize it: compute the average rate over the elapsed window
    /// and fold that into the EWMA. The first solution just primes the
    /// bucket. Returns the share weight so the caller can account the
    /// same solution elsewhere in the same unit.
    pub fn record_share(&self, target: &[u8; 32]) -> u64 {
        let hashes = hashes_per_solution(target);
        let shares = hashes / HASHES_PER_SHARE;
        self.share_count.fetch_add(shares, Ordering::Relaxed);

        let now_ms = Instant::now().as_millis();

        // First share after creation: prime the bucket and return; we
        // need a second share before there is any elapsed time to
        // compute over.
        if !self.bucket_primed.swap(true, Ordering::Relaxed) {
            self.bucket_start_ms.store(now_ms, Ordering::Relaxed);
            self.bucket_hashes.store(hashes, Ordering::Relaxed);
            return shares;
        }

        let bucket_start = self.bucket_start_ms.load(Ordering::Relaxed);
        let elapsed_ms = now_ms.saturating_sub(bucket_start);
        if elapsed_ms < BUCKET_MS {
            self.bucket_hashes.fetch_add(hashes, Ordering::Relaxed);
            return shares;
        }

        // Finalize the bucket. Swap so concurrent readers still see a
        // coherent snapshot.
        let bucket_total = self.bucket_hashes.swap(hashes, Ordering::Relaxed);
        self.bucket_start_ms.store(now_ms, Ordering::Relaxed);
        let stored = self.ewma.load(Ordering::Relaxed);
        // When a share arrives after silence, decay the stored EWMA across
        // the missed buckets first so the resumed value is consistent with
        // what hashrate() was returning during the quiet period.
        let (silent_buckets, fold_window) = resume_split(elapsed_ms);
        let previous = decay_silent(stored, silent_buckets);
        self.ewma.store(
            next_ewma(bucket_total, fold_window, previous),
            Ordering::Relaxed,
        );
        shares
    }

    /// Smoothed hashrate in hashes per second. Zero until the first
    /// bucket finalizes, `BUCKET_MS` after the first share.
    ///
    /// When shares stop arriving the stored EWMA goes stale because
    /// only [`record_share`](Self::record_share) writes it. This
    /// function applies read-side decay: for every full `BUCKET_MS`
    /// window of staleness beyond the first two, it folds a zero-rate
    /// bucket into the EWMA. The result is never written back,
    /// preserving the single-writer invariant.
    #[must_use]
    pub fn hashrate(&self) -> u64 {
        let stored = self.ewma.load(Ordering::Relaxed);
        if stored == 0 || !self.bucket_primed.load(Ordering::Relaxed) {
            return stored;
        }

        let bucket_start = self.bucket_start_ms.load(Ordering::Relaxed);
        let now_ms = Instant::now().as_millis();
        let stale_ms = now_ms.saturating_sub(bucket_start);

        // The current bucket is still accumulating or just barely overdue.
        if stale_ms < 2 * BUCKET_MS {
            return stored;
        }

        let missed = (stale_ms - BUCKET_MS) / BUCKET_MS;
        decay_silent(stored, missed)
    }
}

impl Default for HashrateEstimator {
    fn default() -> Self {
        Self::new()
    }
}

/// Split the gap since the last finalised bucket into the zero-rate
/// buckets to decay across and the window the arriving bucket's hashes
/// accrued over.
///
/// A gap shorter than two buckets is one window with nothing silent
/// behind it. Beyond that, the silent stretch is whole buckets and what
/// is left is the real window, which is where this used to take a whole
/// `BUCKET_MS` instead: shares arrive at random times, so the two agree
/// only when the gap is an exact multiple, and a short window reports
/// the resumed rate high by the ratio between them. The returned window
/// is `elapsed_ms` below two buckets and in `BUCKET_MS..2 * BUCKET_MS`
/// above, so it is zero only if `elapsed_ms` is, which a finalising
/// bucket never is.
fn resume_split(elapsed_ms: u64) -> (u64, u64) {
    if elapsed_ms < 2 * BUCKET_MS {
        return (0, elapsed_ms);
    }
    let silent_buckets = (elapsed_ms - BUCKET_MS) / BUCKET_MS;
    (silent_buckets, elapsed_ms - silent_buckets * BUCKET_MS)
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

/// Expected hashes behind one solution found against `target`, from
/// the leading-zero bits of the 256-bit little-endian target. Exact
/// for the power-of-two difficulties the chip drivers program, and
/// within a factor of two of `2^256 / (target+1)` for any other.
fn hashes_per_solution(target: &[u8; 32]) -> u64 {
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
