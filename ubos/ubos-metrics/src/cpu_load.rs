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

use embassy_time::{Duration, Instant, Ticker};
use portable_atomic::{AtomicU64, Ordering};

use crate::metric;
use crate::value::Ratio;

/// Most cores tracked; a board passes its own count to [`run_cpu_load`].
pub const MAX_CPUS: usize = 2;

/// Ticks spent halted in each core's idle wait.
static IDLE_TICKS: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];

/// Called by core `cpu`'s idle hook once per wake.
pub fn record_idle(cpu: usize, ticks: u64) {
    IDLE_TICKS[cpu].fetch_add(ticks, Ordering::Relaxed);
}

/// Window length over which the published ratio is averaged.
const WINDOW: Duration = Duration::from_secs(5);

/// Publish `cpu_busy_ratio` once per `WINDOW` for each core.
pub async fn run_cpu_load<const N_CPUS: usize>() -> ! {
    const { assert!(N_CPUS <= MAX_CPUS, "N_CPUS exceeds MAX_CPUS") };

    let mut ticker = Ticker::every(WINDOW);
    let mut prev_idle: [u64; N_CPUS] =
        core::array::from_fn(|index| IDLE_TICKS[index].load(Ordering::Relaxed));
    let mut prev_instant = Instant::now();

    loop {
        ticker.next().await;
        // Sampled up front, so every core shares one elapsed window
        // despite the awaits between publishes.
        let now_idle: [u64; N_CPUS] =
            core::array::from_fn(|index| IDLE_TICKS[index].load(Ordering::Relaxed));
        let now_instant = Instant::now();
        let elapsed = (now_instant - prev_instant).as_ticks();

        for cpu_index in 0..N_CPUS {
            let idle = now_idle[cpu_index].wrapping_sub(prev_idle[cpu_index]);
            let busy = Ratio::new(elapsed.saturating_sub(idle), elapsed);

            metric!("cpu_busy_ratio", busy)
                .label("cpu", cpu_index)
                .push()
                .await;

            prev_idle[cpu_index] = now_idle[cpu_index];
        }

        prev_instant = now_instant;
    }
}

#[cfg(target_arch = "arm")]
pub use cortex_m_executor::run_cortex_m_executor;

/// The Cortex-M executor loop that feeds [`record_idle`].
#[cfg(target_arch = "arm")]
mod cortex_m_executor {
    use embassy_executor::Spawner;
    use embassy_time::Instant;
    use static_cell::StaticCell;

    use super::{MAX_CPUS, record_idle};

    /// The pender context `embassy-executor` treats as thread mode, for
    /// which it issues `sev` and so breaks the `wfe` below.
    const THREAD_PENDER: usize = usize::MAX;

    /// Drive a `raw::Executor` on Cortex-M, recording the time halted in
    /// `wfe` for `CPU_INDEX`. `spawn` runs once with the [`Spawner`].
    pub fn run_cortex_m_executor<const CPU_INDEX: usize, F>(spawn: F) -> !
    where
        F: FnOnce(Spawner),
    {
        // A function-local static is shared across generic instantiations,
        // so each `CPU_INDEX` takes its own array slot.
        static EXECUTORS: [StaticCell<embassy_executor::raw::Executor>; MAX_CPUS] =
            [const { StaticCell::new() }; MAX_CPUS];
        const { assert!(CPU_INDEX < MAX_CPUS, "CPU_INDEX exceeds MAX_CPUS") };
        let executor = EXECUTORS[CPU_INDEX].init(embassy_executor::raw::Executor::new(
            THREAD_PENDER as *mut (),
        ));
        let spawner = executor.spawner();
        spawn(spawner);
        loop {
            // SAFETY: this loop is the sole caller of the non-reentrant
            // `poll`.
            unsafe { executor.poll() };
            let before = Instant::now();
            cortex_m::asm::wfe();
            let after = Instant::now();
            record_idle(CPU_INDEX, (after - before).as_ticks());
        }
    }
}
