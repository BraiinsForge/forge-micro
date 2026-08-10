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

//! Ratio-based work scheduler.
//!
//! [`Scheduler`] is the policy interface the serial worker will consult
//! to pick which pool slot to dispatch work from next.
//! [`RatioScheduler`] is its clamped deficit-counter implementation: it
//! splits delivered work between the configured slots at a fixed
//! ratio and routes around unavailable pools.

#[cfg(test)]
#[path = "tests/scheduler.rs"]
mod tests;

/// Number of shares a clamped credit may lag or lead its zero point
/// by, expressed as a multiple of the weight sum. Bounds how long a
/// pool returning from an outage gets repaid before parity resumes.
const CLAMP_SHARES: i32 = 1000;

/// Policy that decides which configured pool slot receives the next
/// unit of dispatched work.
pub trait Scheduler<const N: usize> {
    /// Accounts one difficulty-1 share found on work dispatched for
    /// `slot`.
    fn accumulate_share(&mut self, slot: usize);

    /// Picks the slot to dispatch next, restricted to `available`
    /// slots. Returns `None` when nothing is dispatchable.
    fn evaluate(&mut self, available: &[bool; N]) -> Option<usize>;

    /// Replace the dispatch weights at runtime. The default is a
    /// no-op so policy-free implementations and test stubs need
    /// not care.
    fn set_weights(&mut self, _weights: &[u32]) {}
}

/// Clamped deficit-counter scheduler over `N` pool slots.
///
/// Each slot carries a `weight` and a `credit`. Crediting a share adds
/// every slot's weight to its credit, then subtracts the weight sum
/// from the credited slot, so the update is zero-sum and the
/// delivered share split converges to `weights`. Credits are clamped
/// to bound how much a slot that returns from an outage is repaid. A
/// weight of zero marks a slot as a backup, picked only when no
/// weighted slot has work available.
pub struct RatioScheduler<const N: usize> {
    weights: [u32; N],
    credit: [i32; N],
    weight_sum: i32,
}

impl<const N: usize> RatioScheduler<N> {
    /// Builds a scheduler at the fallback default: slot 0 weighted 1,
    /// every other slot weight 0, credits at zero. Callers that need a
    /// real dispatch ratio configure it through
    /// [`Scheduler::set_weights`] once it is known.
    #[must_use]
    pub fn new() -> Self {
        let mut scheduler = Self {
            weights: [0; N],
            credit: [0; N],
            weight_sum: 0,
        };
        // An empty input truncates to all zero, so this applies the
        // fallback default and derives `weight_sum` through the same
        // path `set_weights` uses at runtime.
        scheduler.set_weights(&[]);
        scheduler
    }
}

impl<const N: usize> Default for RatioScheduler<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> Scheduler<N> for RatioScheduler<N> {
    fn accumulate_share(&mut self, slot: usize) {
        for (credit, &weight) in self.credit.iter_mut().zip(&self.weights) {
            *credit += i32::try_from(weight).expect("weight fits in i32: clamped in set_weights");
        }
        self.credit[slot] -= self.weight_sum;

        let clamp = self.weight_sum * CLAMP_SHARES;
        for credit in &mut self.credit {
            *credit = (*credit).clamp(-clamp, clamp);
        }
    }

    fn evaluate(&mut self, available: &[bool; N]) -> Option<usize> {
        let mut best: Option<usize> = None;
        for (slot, (&is_available, &weight)) in available.iter().zip(&self.weights).enumerate() {
            if !is_available || weight == 0 {
                continue;
            }
            let better = match best {
                None => true,
                Some(current_best) => self.credit[slot] > self.credit[current_best],
            };
            if better {
                best = Some(slot);
            }
        }
        if best.is_some() {
            return best;
        }

        // No weighted slot is available: fall back to the
        // lowest-index available backup slot.
        available
            .iter()
            .zip(&self.weights)
            .position(|(&is_available, &weight)| is_available && weight == 0)
    }

    fn set_weights(&mut self, weights: &[u32]) {
        // Largest weight a slot may hold without letting
        // `weight_sum * (CLAMP_SHARES + 1)` overflow `i32`. Applied to
        // every stored weight, not only their sum: `accumulate_share`
        // converts each slot's own weight to `i32` on every share, so
        // the per-slot bound is what keeps that conversion total.
        // Config validation already bounds weights to 100 000, so this
        // saturation is defense in depth against direct misuse of the
        // method rather than a path with defined precision
        // requirements.
        let max_weight = i64::from(i32::MAX) / i64::from(CLAMP_SHARES + 1);

        let mut new_weights = [0u32; N];
        let len = weights.len().min(N);
        for (dst, &src) in new_weights[..len].iter_mut().zip(&weights[..len]) {
            let clamped = i64::from(src).min(max_weight);
            *dst = u32::try_from(clamped).expect("clamped to max_weight, which fits in u32");
        }

        // Post-truncation fallback: a reduced-slot board whose only
        // weighted entry lies beyond its slot count still mines slot 0.
        if N > 0 && new_weights.iter().all(|&weight| weight == 0) {
            new_weights[0] = 1;
        }

        let weight_sum_i64: i64 = new_weights.iter().map(|&weight| i64::from(weight)).sum();
        let weight_sum_i64 = weight_sum_i64.min(max_weight);
        self.weight_sum =
            i32::try_from(weight_sum_i64).expect("clamped to max_weight, which fits in i32");

        self.weights = new_weights;
        // A reweight is a clean slate by design: it coincides with the
        // stratum sessions restarting on the same config generation.
        self.credit = [0; N];
    }
}

/// Maps a configured pool list to the `[u32; 8]` shape
/// [`crate::serial::Serial::set_pool_weights`] takes: each entry's
/// weight lands in its slot index, and slots beyond the list stay
/// zero. A pure helper so the mapping is unit-testable independently
/// of the watcher's task loop.
#[must_use]
pub fn pool_weights(pool: &[ubos_config::PoolEntry]) -> [u32; 8] {
    let mut weights = [0u32; 8];
    for (slot, entry) in weights.iter_mut().zip(pool) {
        *slot = entry.weight;
    }
    weights
}

/// Watches the config generation and pushes the mapped pool weights
/// to the serial actor whenever they actually change.
///
/// The `last_sent` equality guard means unrelated config updates
/// (thermal, fan) never disturb the scheduler: only an edit that
/// changes a pool's weight reaches it. The first iteration runs
/// before the first `changed()` await, so the configured weights
/// reach the scheduler at boot rather than waiting for a later
/// config edit.
pub async fn run_pool_weights_watcher() -> ! {
    let handle = ubos_config::handle();
    let mut generation = handle.subscribe_generation();
    let mut last_sent: Option<[u32; 8]> = None;
    loop {
        let weights = pool_weights(&handle.current().pool);
        if last_sent != Some(weights) {
            crate::serial::SERIAL.set_pool_weights(weights).await;
            last_sent = Some(weights);
        }
        generation.changed().await;
    }
}

/// Task shell around [`run_pool_weights_watcher`], which watches the
/// configured pool weights and re-derives the work split.
#[macro_export]
macro_rules! pool_weights_task {
    () => {
        #[embassy_executor::task]
        async fn pool_weights_task() -> ! {
            ubos_mining::scheduler::run_pool_weights_watcher().await
        }
    };
}
