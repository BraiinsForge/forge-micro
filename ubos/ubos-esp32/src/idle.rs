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

//! Idle-accounting executor callbacks.

use embassy_time::Instant;
use esp_rtos::embassy::Callbacks;

/// Brackets `flags.wait()` inside
/// `esp_rtos::embassy::Executor::run_with_callbacks`, which fires `on_idle`
/// only when the wait will actually block, so spurious wakes do not skew the
/// idle ticks recorded for `CPU_INDEX`.
pub struct IdleAccounting<const CPU_INDEX: usize> {
    idle_start: Option<Instant>,
}

impl<const CPU_INDEX: usize> IdleAccounting<CPU_INDEX> {
    #[must_use]
    pub const fn new() -> Self {
        const {
            assert!(
                CPU_INDEX < ubos_metrics::cpu_load::MAX_CPUS,
                "CPU_INDEX exceeds MAX_CPUS"
            );
        }
        Self { idle_start: None }
    }
}

impl<const CPU_INDEX: usize> Default for IdleAccounting<CPU_INDEX> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const CPU_INDEX: usize> Callbacks for IdleAccounting<CPU_INDEX> {
    fn before_poll(&mut self) {
        if let Some(start) = self.idle_start.take() {
            let elapsed = (Instant::now() - start).as_ticks();
            ubos_metrics::cpu_load::record_idle(CPU_INDEX, elapsed);
        }
    }

    fn on_idle(&mut self) {
        self.idle_start = Some(Instant::now());
    }
}
