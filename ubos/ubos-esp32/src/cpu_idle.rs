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

//! The idle time of each core, and the sampler that turns it into busy
//! ticks.
//!
//! The executor calls [`record_idle`] each time its core
//! wakes from a halt. A task reads the busy ticks of the last window
//! through [`CpuSampler`], and pushes them as a metric.
//!
//! Model and caveats: `../../../docs/METRICS.md`.

use embassy_time::Instant;
use portable_atomic::{AtomicU64, Ordering};

/// Most cores tracked.
pub(crate) const MAX_CPUS: usize = 2;

/// Ticks spent halted in each core's idle wait.
static IDLE_TICKS: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];

/// Called by core `cpu`'s idle hook once per wake.
pub(crate) fn record_idle(cpu: usize, ticks: u64) {
    IDLE_TICKS[cpu].fetch_add(ticks, Ordering::Relaxed);
}

/// The busy time of each core over one window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CpuSample<const N_CPUS: usize> {
    /// Ticks from the last sample to this one. It is the same for every
    /// core.
    pub(crate) elapsed_ticks: u64,
    /// Ticks that each core did not spend halted in the window.
    pub(crate) busy_ticks: [u64; N_CPUS],
}

/// Reads the idle counters of the first `N_CPUS` cores, and gives the
/// busy ticks since its last sample.
pub(crate) struct CpuSampler<const N_CPUS: usize> {
    prev_idle: [u64; N_CPUS],
    prev_ticks: u64,
}

impl<const N_CPUS: usize> CpuSampler<N_CPUS> {
    /// Start the first window now.
    #[must_use]
    pub(crate) fn new() -> Self {
        const { assert!(N_CPUS <= MAX_CPUS, "N_CPUS exceeds MAX_CPUS") };
        Self {
            prev_idle: read_idle(),
            prev_ticks: Instant::now().as_ticks(),
        }
    }

    /// Close the window and start the next one. All cores are read
    /// together, so they share one elapsed window.
    pub(crate) fn sample(&mut self) -> CpuSample<N_CPUS> {
        let idle = read_idle();
        self.advance(idle, Instant::now().as_ticks())
    }

    fn advance(&mut self, idle: [u64; N_CPUS], now_ticks: u64) -> CpuSample<N_CPUS> {
        let elapsed_ticks = now_ticks.saturating_sub(self.prev_ticks);
        // A core that idled for longer than the window, as a wake can
        // end after the sample, counts as fully idle.
        let busy_ticks = core::array::from_fn(|cpu| {
            elapsed_ticks.saturating_sub(idle[cpu].wrapping_sub(self.prev_idle[cpu]))
        });
        self.prev_idle = idle;
        self.prev_ticks = now_ticks;
        CpuSample {
            elapsed_ticks,
            busy_ticks,
        }
    }
}

impl<const N_CPUS: usize> Default for CpuSampler<N_CPUS> {
    fn default() -> Self {
        Self::new()
    }
}

fn read_idle<const N_CPUS: usize>() -> [u64; N_CPUS] {
    core::array::from_fn(|cpu| IDLE_TICKS[cpu].load(Ordering::Relaxed))
}
