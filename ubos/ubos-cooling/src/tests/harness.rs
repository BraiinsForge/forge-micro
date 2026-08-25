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

//! Scaffolding shared by the tests that drive [`crate::run_cooling`]
//! against `embassy_time::MockDriver`.
//!
//! The task never returns, so it is polled manually instead of run under an
//! executor: each phase advances the mock clock or publishes a thermal
//! reading, then polls `run_cooling` and the metrics ingest task a bounded
//! number of times to let the resulting chain of awaits settle before the
//! commanded PWM is asserted through [`crate::handle`].

// The crate is `no_std`; the host test binary links std for the mutex that
// serialises the tests driving `run_cooling`.
extern crate std;

use core::cell::Cell;
use core::future::Future;
use core::pin::Pin;
use core::task::Context;

use ubos_core::units::{FanPwm, FanRpm, Temperature};
use ubos_thermal::ThermalReading;

use crate::{FanOutput, TachReader};

/// Serialises the tests that drive `run_cooling`.
///
/// They share every static the task touches — the mock clock, the config,
/// thermal and fan watches, and the metrics channel — so two of them running
/// concurrently would see each other's clock resets and published values.
/// Holding this mutex for the whole test body keeps them sequential without
/// needing `--test-threads=1`. A test that publishes a non-default config
/// must restore the defaults before releasing it, through a `Drop` guard
/// declared after the lock so it runs on both the normal path and an
/// unwind. A caller recovers a poisoned lock with `into_inner` rather than
/// `expect`, because by the time it does, that guard has already restored
/// the statics the panicking test left dirty; propagating the poison would
/// only replace the failing test's own assertion with a message about the
/// lock instead.
pub(crate) static LOOP_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Number of times each phase polls `run_cooling` and the metrics ingest
/// task, enough for the awaits a phase triggers to settle — the metric pushes
/// and any catch-up control ticks — while staying a fixed bound rather than an
/// open-ended poll loop. The bound assumes the default config's thermal sample
/// interval: a phase advances the clock by one `stale_timeout`, so raising
/// that interval enough that the window spans more than `POLL_BUDGET` control
/// ticks would under-poll and need a larger bound.
const POLL_BUDGET: usize = 64;

/// Fan that latches the first duty commanded to it.
///
/// The duty `run_cooling` starts on never reaches the fan watch: the seeded
/// reading resolves the thermal arm inside the same [`drive`], so the control
/// pass overwrites the boot publish before the phase can read the watch. What
/// reached the fan is where the boot branch is observable, and it is latched
/// rather than tracked because the boot publish precedes the thermal
/// subscription, so the first command is the boot duty whatever the loop
/// commands afterwards.
pub(crate) struct RecordingFan<'a>(pub(crate) &'a Cell<Option<FanPwm>>);

impl FanOutput for RecordingFan<'_> {
    fn set_pwm(&mut self, pwm: FanPwm) {
        if self.0.get().is_none() {
            self.0.set(Some(pwm));
        }
    }
}

pub(crate) struct FakeTach;

impl TachReader<1> for FakeTach {
    const LABELS: [Option<&'static str>; 1] = [None];

    async fn read_rpms(&mut self) -> [FanRpm; 1] {
        [FanRpm::from_rpm(0)]
    }
}

pub(crate) fn moderate_reading(sensor_fault: bool) -> ThermalReading {
    // Well below the default 67°C target: the PID drives the PWM toward
    // its floor rather than toward FULL, so a non-fault reading at this
    // temperature unambiguously demonstrates the fan leaving FULL.
    let temp = Temperature::from_millicelsius(50_000);
    ThermalReading {
        chip_max: temp,
        pid_temp: temp,
        timestamp_ms: 0,
        sensor_fault,
    }
}

/// Polls `cooling` and `ingest` up to [`POLL_BUDGET`] times so the awaits
/// triggered by the last clock advance or published reading settle. Both
/// futures never complete — `run_cooling` is `-> !`, `run_ingest` is `-> !`
/// and the channel never empties on its own — so the return value carries
/// no information; only the side effects on `crate::STATE_WATCH` matter.
/// Takes the pinned futures by reborrow so the caller's `pin!` bindings
/// stay usable across the repeated calls one per test phase.
pub(crate) fn drive<Cooling: Future, Ingest: Future>(
    mut cooling: Pin<&mut Cooling>,
    mut ingest: Pin<&mut Ingest>,
    cx: &mut Context<'_>,
) {
    for _ in 0..POLL_BUDGET {
        let _ = cooling.as_mut().poll(cx);
        let _ = ingest.as_mut().poll(cx);
    }
}
