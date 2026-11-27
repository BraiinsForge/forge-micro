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

//! The task that publishes the busy ratio of each core.
//!
//! Model and caveats: `../../../docs/METRICS.md`.

use crate::cpu_idle::CpuSampler;
use embassy_time::{Duration, Ticker};
use ubos_metrics::metric;
use ubos_metrics::value::Ratio;

/// Window length over which the published ratio is averaged.
const WINDOW: Duration = Duration::from_secs(5);

/// Publish `cpu_busy_ratio` once per window for each of `N_CPUS` cores.
pub async fn run_cpu_load<const N_CPUS: usize>() -> ! {
    let mut ticker = Ticker::every(WINDOW);
    let mut sampler = CpuSampler::<N_CPUS>::new();
    loop {
        ticker.next().await;
        let sample = sampler.sample();
        for (cpu, busy) in sample.busy_ticks.into_iter().enumerate() {
            metric!("cpu_busy_ratio", Ratio::new(busy, sample.elapsed_ticks))
                .label("cpu", cpu)
                .push()
                .await;
        }
    }
}

/// Task shell around [`run_cpu_load`].
///
/// See `ubos_firmware::tasks` for why this is a macro rather than a
/// generic function.
#[macro_export]
macro_rules! cpu_load_task {
    ($n_cpus:expr) => {
        #[embassy_executor::task]
        async fn cpu_load_task() -> ! {
            ubos_esp32::cpu_load::run_cpu_load::<{ $n_cpus }>().await
        }
    };
}
