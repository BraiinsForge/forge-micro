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
    /// Builds a scheduler for `weights`. Credits all start at zero.
    ///
    /// # Panics
    ///
    /// Panics if every weight is zero, or if the weight sum is large
    /// enough that the credit clamp would not fit in `i32`.
    #[must_use]
    pub fn new(weights: [u32; N]) -> Self {
        assert!(
            weights.iter().any(|&weight| weight > 0),
            "RatioScheduler requires at least one non-zero weight"
        );

        let weight_sum_i64: i64 = weights.iter().map(|&weight| i64::from(weight)).sum();
        let max_magnitude = weight_sum_i64 * i64::from(CLAMP_SHARES + 1);
        assert!(
            max_magnitude <= i64::from(i32::MAX),
            "RatioScheduler weight sum too large for the credit clamp to fit i32"
        );
        let weight_sum =
            i32::try_from(weight_sum_i64).expect("checked above: weight sum fits in i32");

        Self {
            weights,
            credit: [0; N],
            weight_sum,
        }
    }
}

impl<const N: usize> Scheduler<N> for RatioScheduler<N> {
    fn accumulate_share(&mut self, slot: usize) {
        for (credit, &weight) in self.credit.iter_mut().zip(&self.weights) {
            *credit += i32::try_from(weight).expect("weight fits in i32: checked in Self::new");
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
}
