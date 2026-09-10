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

//! CPU busy ratio published as a Prometheus gauge.
//!
//! Model and caveats: `../../../docs/METRICS.md`.

use core::num::NonZeroU64;

use embassy_time::{Duration, Instant, Ticker};
use portable_atomic::{AtomicU64, Ordering};

use crate::metrics::{Label, LabelValue, Labels, Metric, Value, push};

/// Maximum number of cores tracked by [`record_idle`]. Each board
/// declares how many of these slots it populates by passing `N_CPUS`
/// to [`run_cpu_load`].
pub const MAX_CPUS: usize = 2;

/// Total `embassy_time` ticks spent halted in each core's idle wait.
/// Boards add to slot `N` from the idle hook running on core `N`.
static IDLE_TICKS: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];

/// Adds `ticks` spent halted on core `cpu` to that core's idle total. An
/// idle hook calls this once per wake with the ticks it measured.
pub fn record_idle(cpu: usize, ticks: u64) {
    IDLE_TICKS[cpu].fetch_add(ticks, Ordering::Relaxed);
}

/// Static `cpu` label values, indexed by core ID. Avoids per-window
/// integer-to-string formatting on the publish path.
const CPU_LABELS: [&str; MAX_CPUS] = ["0", "1"];

/// Window length over which the published ratio is averaged, paired
/// with its `window` label string. Co-located so changing the cadence
/// forces the dashboard label to follow.
const WINDOW: (Duration, &str) = (Duration::from_secs(5), "5s");

/// Publish `board_cpu_busy_ratio` once per [`WINDOW`] for each of the
/// `N_CPUS` active cores.
///
/// Call this from a firmware `#[embassy_executor::task]`. `N_CPUS`
/// must be at most [`MAX_CPUS`].
pub async fn run_cpu_load<const N_CPUS: usize>() -> ! {
    const { assert!(N_CPUS <= MAX_CPUS, "N_CPUS exceeds MAX_CPUS") };

    let mut ticker = Ticker::every(WINDOW.0);
    let mut prev_idle: [u64; N_CPUS] =
        core::array::from_fn(|index| IDLE_TICKS[index].load(Ordering::Relaxed));
    let mut prev_instant = Instant::now();

    loop {
        ticker.next().await;
        // Sample every core's idle counter up front so all per-cpu
        // ratios share the same elapsed window. Publishing awaits
        // between cores, so a per-iteration sample would skew later
        // cores against an already-stale `now_instant`.
        let now_idle: [u64; N_CPUS] =
            core::array::from_fn(|index| IDLE_TICKS[index].load(Ordering::Relaxed));
        let now_instant = Instant::now();
        let elapsed = (now_instant - prev_instant).as_ticks();

        for cpu_index in 0..N_CPUS {
            let idle = now_idle[cpu_index].wrapping_sub(prev_idle[cpu_index]);
            let busy = busy_ratio(idle, elapsed);

            let mut labels = Labels::new();
            // Window and cpu labels are fixed and known to fit; pushes cannot fail.
            let _ = labels.push(Label {
                name: "window",
                value: LabelValue::Static(WINDOW.1),
            });
            let _ = labels.push(Label {
                name: "cpu",
                value: LabelValue::Static(CPU_LABELS[cpu_index]),
            });
            push(Metric::with_labels(
                "board_cpu_busy_ratio",
                labels,
                Value::Float(busy),
            ))
            .await;

            prev_idle[cpu_index] = now_idle[cpu_index];
        }

        prev_instant = now_instant;
    }
}

/// Convert idle and elapsed tick counts into a clamped 0..=1 busy
/// ratio. Returns 0.0 when no time has elapsed since the last sample.
fn busy_ratio(idle: u64, elapsed: u64) -> f32 {
    let Some(elapsed) = NonZeroU64::new(elapsed) else {
        return 0.0;
    };
    #[allow(clippy::cast_precision_loss)]
    let ratio = idle as f32 / elapsed.get() as f32;
    (1.0 - ratio).clamp(0.0, 1.0)
}

#[cfg(target_arch = "arm")]
pub use cortex_m_executor::run_cortex_m_executor;

/// Cortex-M-specific seam between `cortex_m_rt::entry` and the
/// `raw::Executor` poll/wfe loop that feeds [`record_idle`]. Gated
/// to ARM so the shared library still compiles for ESP32-S3.
#[cfg(target_arch = "arm")]
mod cortex_m_executor {
    use embassy_executor::Spawner;
    use embassy_time::Instant;
    use static_cell::StaticCell;

    use super::{MAX_CPUS, record_idle};

    /// Sentinel value the `embassy-executor` cortex-m pender treats as
    /// thread mode. The pender issues `sev` for this branch, which
    /// breaks the `wfe` below when a task is woken.
    const THREAD_PENDER: usize = usize::MAX;

    /// Drive an embassy `raw::Executor` on Cortex-M, accumulating the
    /// time spent halted in `wfe` through `record_idle` for `CPU_INDEX`.
    ///
    /// `spawn` is invoked once with the executor's [`Spawner`] so the
    /// firmware can spawn its boot task. The function never returns.
    ///
    /// Call this from inside `#[cortex_m_rt::entry] fn main`.
    pub fn run_cortex_m_executor<const CPU_INDEX: usize, F>(spawn: F) -> !
    where
        F: FnOnce(Spawner),
    {
        // Function-local statics are not duplicated per generic
        // instantiation, so we keep a per-`CPU_INDEX` slot in an array
        // and pick the right one at runtime.
        static EXECUTORS: [StaticCell<embassy_executor::raw::Executor>; MAX_CPUS] =
            [const { StaticCell::new() }; MAX_CPUS];
        const { assert!(CPU_INDEX < MAX_CPUS, "CPU_INDEX exceeds MAX_CPUS") };
        let executor = EXECUTORS[CPU_INDEX].init(embassy_executor::raw::Executor::new(
            THREAD_PENDER as *mut (),
        ));
        let spawner = executor.spawner();
        spawn(spawner);
        loop {
            // SAFETY: `raw::Executor::poll` is not reentrant. This loop
            // is the sole caller, and no interrupt handler invokes
            // `poll`.
            unsafe { executor.poll() };
            let before = Instant::now();
            cortex_m::asm::wfe();
            let after = Instant::now();
            record_idle(CPU_INDEX, (after - before).as_ticks());
        }
    }
}
