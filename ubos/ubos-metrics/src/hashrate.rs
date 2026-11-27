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
use ubos_core::units::Shares;

#[cfg(test)]
#[path = "tests/hashrate.rs"]
mod tests;

/// EWMA smoothing exponent: α = 1/8, about an 8-bucket window.
const EWMA_ALPHA_SHIFT: u32 = 3;

/// Rate-accumulation bucket. A per-share instant rate is biased high on
/// a Poisson process, so only a bucket's average rate feeds the EWMA.
const BUCKET_MS: u64 = 1_000;

/// Beyond this many missed buckets the read-side decay clamps to zero;
/// the residual at α = 1/8 rounds to zero in `u64` by then.
const MAX_DECAY_STEPS: u64 = 64;

/// Bucketed EWMA hashrate accounting. Exactly one actor calls
/// [`record_share`](Self::record_share); the load and store sequences
/// below rely on that single writer.
pub struct HashrateEstimator {
    /// Total work, in the unit of [`Shares`].
    share_count: AtomicU64,
    /// Smoothed hashrate in H/s, updated once per finalised bucket.
    ewma: AtomicU64,
    /// Hashes credited to the in-progress bucket.
    bucket_hashes: AtomicU64,
    /// Uptime ms at which the in-progress bucket started; valid once
    /// `bucket_primed` is set.
    bucket_start_ms: AtomicU64,
    /// Gates the first-share branch, so a first share landing at 0 ms
    /// does not retrigger it.
    bucket_primed: AtomicBool,
}

impl HashrateEstimator {
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

    #[must_use]
    pub fn share_count(&self) -> Shares {
        Shares::new(self.share_count.load(Ordering::Relaxed))
    }

    /// Credit one chip solution found against `target`: its difficulty in
    /// shares to the counter, its hashes to the current bucket, which
    /// folds into the EWMA once the bucket has run for `BUCKET_MS`.
    /// Returns the share weight, so the caller can account the same
    /// solution in the same unit.
    pub fn record_share(&self, target: &[u8; 32]) -> Shares {
        let hashes = hashes_per_solution(target);
        let shares = Shares::new(hashes / Shares::HASHES_PER_SHARE);
        self.share_count.fetch_add(shares.get(), Ordering::Relaxed);

        let now_ms = Instant::now().as_millis();

        // The first share only primes the bucket.
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

        let bucket_total = self.bucket_hashes.swap(hashes, Ordering::Relaxed);
        self.bucket_start_ms.store(now_ms, Ordering::Relaxed);
        let stored = self.ewma.load(Ordering::Relaxed);
        // After silence the stored EWMA is decayed first, so the resumed
        // value agrees with what `hashrate()` returned meanwhile.
        let (silent_buckets, fold_window) = resume_split(elapsed_ms);
        let previous = decay_silent(stored, silent_buckets);
        self.ewma.store(
            next_ewma(bucket_total, fold_window, previous),
            Ordering::Relaxed,
        );
        shares
    }

    /// Smoothed hashrate in H/s; zero until the first bucket finalizes.
    /// Decays read-side only once the current bucket is two buckets stale,
    /// and never writes back, so the single-writer invariant holds.
    #[must_use]
    pub fn hashrate(&self) -> u64 {
        let stored = self.ewma.load(Ordering::Relaxed);
        if stored == 0 || !self.bucket_primed.load(Ordering::Relaxed) {
            return stored;
        }

        let bucket_start = self.bucket_start_ms.load(Ordering::Relaxed);
        let now_ms = Instant::now().as_millis();
        let stale_ms = now_ms.saturating_sub(bucket_start);

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

/// Split the gap since the last finalised bucket into the silent buckets
/// to decay across and the window the arriving hashes accrued over. The
/// window is the real remainder, not a whole `BUCKET_MS`, or a resumed
/// rate reads high by the ratio between them. It is never zero for a
/// finalising bucket.
fn resume_split(elapsed_ms: u64) -> (u64, u64) {
    if elapsed_ms < 2 * BUCKET_MS {
        return (0, elapsed_ms);
    }
    let silent_buckets = (elapsed_ms - BUCKET_MS) / BUCKET_MS;
    (silent_buckets, elapsed_ms - silent_buckets * BUCKET_MS)
}

/// Apply `missed` zero-rate EWMA steps to `value`; see
/// [`MAX_DECAY_STEPS`].
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

/// Fold a finalized bucket into the EWMA. The `i128` delta carries the
/// signed difference of two `u64` values without overflow.
fn next_ewma(bucket_total: u64, elapsed_ms: u64, previous: u64) -> u64 {
    let bucket_rate = bucket_total.saturating_mul(1000) / elapsed_ms;
    let delta = i128::from(bucket_rate) - i128::from(previous);
    let next = i128::from(previous) + (delta >> EWMA_ALPHA_SHIFT);
    u64::try_from(next.max(0)).unwrap_or(u64::MAX)
}

/// `Display` adapter for a hashrate: `"<n> H/s"` below 1 GH/s,
/// `"<n>.<dd> GH/s"` above.
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

/// Expected hashes behind one solution found against `target`, from the
/// leading-zero bits of the 256-bit little-endian target. Exact for the
/// power-of-two difficulties the chip drivers program, and within a factor
/// of two of `2^256 / (target+1)` for any other.
fn hashes_per_solution(target: &[u8; 32]) -> u64 {
    let mut bits: u32 = 0;
    // Little-endian, so the most significant byte is at index 31.
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
