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

//! The Cortex-M executor shell that records the idle time of its core.
//!
//! Model and caveats: `../../../docs/METRICS.md`.

use crate::cpu_idle::{MAX_CPUS, record_idle};
use embassy_executor::Spawner;
use embassy_time::Instant;
use static_cell::StaticCell;

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
