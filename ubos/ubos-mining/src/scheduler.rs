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
//! ratio and routes around unavailable pools. [`FeeScheduler`] wraps
//! it and holds the fee slot at a fixed share of the delivered work.

#[cfg(test)]
#[path = "tests/scheduler.rs"]
mod tests;

use ubos_config::{DEVFEE, FEE_DEN, MIN_FEE_SLOTS, REPAY_SPACING};

/// Number of shares a clamped credit may lag or lead its zero point
/// by, expressed as a multiple of the weight sum. Bounds how long a
/// pool returning from an outage gets repaid before parity resumes.
const CLAMP_SHARES: i32 = 1000;

/// Fee shares the debt of a [`FeeScheduler`] can lag or lead by: the
/// same trade [`CLAMP_SHARES`] makes for an operator pool.
const CAP_FEE_SHARES: i64 = 1000;

/// Policy that decides which configured pool slot receives the next
/// unit of dispatched work.
pub trait Scheduler<const N: usize> {
    /// Accounts one difficulty-1 share found on work dispatched for
    /// `slot`.
    fn accumulate_share(&mut self, slot: usize);

    /// Picks the slot to dispatch next, restricted to `available`
    /// slots. Returns `None` when nothing is dispatchable.
    ///
    /// The pick changes no state, so the same call answers whether any
    /// work is dispatchable without dispatching it.
    fn evaluate(&self, available: &[bool; N]) -> Option<usize>;

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

    fn evaluate(&self, available: &[bool; N]) -> Option<usize> {
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
        // `weight_sum * (CLAMP_SHARES + 1)` overflow `i32`; applied to
        // every stored weight, not only their sum, because
        // `accumulate_share` converts each slot's own weight to `i32`
        // on every share. Config validation already bounds weights to
        // 100 000, so this saturation is defense in depth against
        // direct misuse of the method.
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

/// A [`RatioScheduler`] with the board's last slot, the fee slot, held
/// at a fixed share of the delivered shares.
///
/// Each operator share adds the fee's numerator to a debt and each fee
/// share subtracts the rest of the denominator, so the split is exact in
/// the long run. The fee slot stays outside the inner scheduler's view,
/// so a chain with no operator pool available idles.
///
/// A counter rather than a weight, because the inner scheduler's clamp
/// bends a small fixed fraction when the operator weights are large, and
/// a derived fee weight rounds to zero, which means backup only, when
/// they are small.
///
/// An off build, or one with fewer than [`MIN_FEE_SLOTS`] slots, runs the
/// operator split alone, so a fee board names this scheduler in both
/// states and the switch decides.
pub struct FeeScheduler<const N: usize> {
    inner: RatioScheduler<N>,
    /// Thousandths of a share the fee slot is owed. Grows while the fee
    /// pool is down, so a returning fee pool is repaid up to the cap.
    debt: i64,
    permille: i64,
    /// Paces a repayment: a fee share is picked only after
    /// [`REPAY_SPACING`] operator shares.
    since_fee: i64,
}

impl<const N: usize> FeeScheduler<N> {
    const FEE_SLOT_RUNS: bool = DEVFEE && N >= MIN_FEE_SLOTS;

    /// The rule `ubos_config::FeeSlot::last_of` states.
    const FEE_SLOT: usize = N - 1;

    /// Starts with no debt, so the first fee share follows the
    /// operator's first run.
    ///
    /// # Panics
    ///
    /// On a share outside 0 to [`FEE_DEN`] exclusive or past
    /// [`ubos_config::MAX_FEE_PERMILLE`]. `ubos_config::FeePool` bounds
    /// the share by the same constants at build time, so either is a
    /// wiring bug.
    #[must_use]
    pub fn new(permille: i64) -> Self {
        assert!(
            (0..FEE_DEN).contains(&permille),
            "BUG: a fee share is a number from 0 to {}",
            FEE_DEN - 1
        );
        assert!(
            permille * (REPAY_SPACING + 1) < FEE_DEN,
            "BUG: a fee share of {} per mille or more cannot repay a debt",
            FEE_DEN / (REPAY_SPACING + 1)
        );
        Self {
            inner: RatioScheduler::new(),
            debt: 0,
            permille,
            since_fee: 0,
        }
    }

    /// Debt at which one fee share is due.
    fn due(&self) -> i64 {
        FEE_DEN - self.permille
    }

    fn cap(&self) -> i64 {
        self.due() * CAP_FEE_SHARES
    }
}

impl<const N: usize> Scheduler<N> for FeeScheduler<N> {
    fn accumulate_share(&mut self, slot: usize) {
        if !Self::FEE_SLOT_RUNS {
            self.inner.accumulate_share(slot);
            return;
        }
        if slot == Self::FEE_SLOT {
            self.debt = (self.debt - self.due()).max(-self.cap());
            self.since_fee = 0;
            return;
        }
        self.debt = (self.debt + self.permille).min(self.cap());
        self.since_fee = self.since_fee.saturating_add(1);
        self.inner.accumulate_share(slot);
    }

    fn evaluate(&self, available: &[bool; N]) -> Option<usize> {
        if !Self::FEE_SLOT_RUNS {
            return self.inner.evaluate(available);
        }
        let mut operator = *available;
        operator[Self::FEE_SLOT] = false;
        // The fee is a share of the operator's work, so no operator
        // pool means no mining.
        let pick = self.inner.evaluate(&operator)?;
        let due = self.debt >= self.due();
        let paced = self.since_fee >= REPAY_SPACING;
        if due && paced && available[Self::FEE_SLOT] {
            return Some(Self::FEE_SLOT);
        }
        Some(pick)
    }

    fn set_weights(&mut self, weights: &[u32]) {
        // The debt survives a reweight: an operator who edits the pool
        // list in a loop must not erase the fee.
        if !Self::FEE_SLOT_RUNS {
            self.inner.set_weights(weights);
            return;
        }
        // The operator's list reaches the fee slot on a board with as
        // many sessions as entries, and a weight there would count in
        // the split of a slot the inner scheduler never picks.
        let operator = weights.len().min(Self::FEE_SLOT);
        self.inner.set_weights(&weights[..operator]);
    }
}

/// Maps a configured pool list to the `[u32; MAX_POOL_SLOTS]` shape
/// [`crate::serial::Serial::set_pool_weights`] takes: each entry's
/// weight lands in its slot index, and slots beyond the list stay
/// zero. A pure helper so the mapping is unit-testable independently
/// of the watcher's task loop.
#[must_use]
pub fn pool_weights(pool: &[ubos_config::PoolEntry]) -> [u32; crate::consts::MAX_POOL_SLOTS] {
    let mut weights = [0u32; crate::consts::MAX_POOL_SLOTS];
    for (slot, entry) in weights.iter_mut().zip(pool) {
        *slot = entry.weight;
    }
    weights
}

/// Watches the config generation and pushes the mapped pool weights
/// to every chain in `actors` whenever they actually change.
///
/// The `last_sent` equality guard means unrelated config updates
/// (thermal, fan) never disturb the scheduler: only an edit that
/// changes a pool's weight reaches it. The first iteration runs
/// before the first `changed()` await, so the configured weights
/// reach every chain at boot rather than waiting for a later config
/// edit.
///
/// [`crate::serial::Serial::set_pool_weights`] publishes rather than
/// sending a command, so this loop never waits on an actor: a chain
/// part-way through a bring-up holds its caller mutex for seconds, and a
/// watcher blocked on the first chain would leave the rest of them on
/// the old split for that whole window.
pub async fn run_pool_weights_watcher(
    actors: &'static [crate::serial::Serial],
    config: ubos_config::Handle,
) -> ! {
    let mut generation = config.subscribe_generation();
    let mut last_sent: Option<[u32; crate::consts::MAX_POOL_SLOTS]> = None;
    loop {
        let weights = pool_weights(&config.current().await.pool);
        if last_sent != Some(weights) {
            for actor in actors {
                actor.set_pool_weights(weights);
            }
            last_sent = Some(weights);
        }
        generation.changed().await;
    }
}

/// Task shell around [`run_pool_weights_watcher`], which watches the
/// configured pool weights and re-derives the work split for every
/// chain in `actors`.
#[macro_export]
macro_rules! pool_weights_task {
    ($actors:expr) => {
        #[embassy_executor::task]
        async fn pool_weights_task(config: ubos_config::Handle) -> ! {
            ubos_mining::scheduler::run_pool_weights_watcher($actors, config).await
        }
    };
}
