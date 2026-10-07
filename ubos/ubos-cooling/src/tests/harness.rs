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
use core::task::{Context, Waker};

use ubos_config::{
    ConfigCommandChannel, ConfigWatch, GenerationWatch, Handle as ConfigHandle, NetworkWatch,
};
use ubos_core::units::{FanPwm, FanRpm, Temperature};
use ubos_thermal::{Handle as ThermalHandle, ThermalReading, ThermalWatch};

use crate::{FanOutput, TachReader};

/// The upstream channels one test drives, standing in for the thermal
/// sensor task and the config task.
///
/// Each test file declares its own, so nothing one publishes reaches
/// another. A single set shared between test files is what used to make
/// one test depend on another having reset the config it left behind.
pub(crate) struct Upstream {
    thermal: ThermalWatch,
    config: ConfigWatch,
    network: NetworkWatch,
    commands: ConfigCommandChannel,
    generation: GenerationWatch,
}

impl Upstream {
    pub(crate) const fn new() -> Self {
        Self {
            thermal: ThermalWatch::new(),
            config: ConfigWatch::new(),
            network: NetworkWatch::new(),
            commands: ConfigCommandChannel::new(),
            generation: GenerationWatch::new(),
        }
    }

    /// The thermal handle `run_cooling` takes.
    pub(crate) fn thermal(&'static self) -> ThermalHandle {
        ThermalHandle::new(&self.thermal)
    }

    /// The config handle `run_cooling` takes.
    pub(crate) fn config(&'static self) -> ConfigHandle {
        ConfigHandle::new(
            &self.config,
            &self.network,
            &self.commands,
            &self.generation,
        )
    }

    /// Publish a reading, as the sensor task would.
    pub(crate) fn publish_reading(&self, reading: ThermalReading) {
        self.thermal.sender().send(reading);
    }

    /// Publish a config, as the config task would after it persisted.
    pub(crate) fn publish_config(&self, config: ubos_config::Config) {
        self.config.sender().send(config);
    }
}

/// Serialises the tests that drive `run_cooling`.
///
/// Each test owns its thermal and config channels through [`Upstream`],
/// so those no longer need guarding. Three statics remain shared and
/// cannot be owned per test: `embassy_time::MockDriver`, which is one
/// instance per process with no second one to construct, this crate's
/// fan watch, and the `ubos-metrics` channel that `run_ingest` drains.
/// Two tests running concurrently would see each other's clock resets
/// and published duties. Holding this mutex for the whole test body
/// keeps them sequential without needing `--test-threads=1`. A caller
/// recovers a poisoned lock with `into_inner` rather than `expect`,
/// because a poison only means an earlier test panicked, and
/// propagating it would replace this test's own assertion with a
/// message about the lock.
pub(crate) static LOOP_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Series slots in the map the ingest task fills. The tests read no
/// exposition, so any power of two serves.
const MAX_METRICS: usize = 64;

/// Bytes of the exposition buffer, which no test renders into.
const METRICS_RESPONSE_BYTES: usize = 4096;

/// The metrics map and exposition buffer `run_ingest` writes into.
///
/// One store serves every test, because [`LOOP_MUTEX`] already keeps the
/// tests sequential and the channel feeding it is shared the same way.
pub(crate) static METRICS_STATICS: ubos_metrics::metrics::MetricsStatics<
    MAX_METRICS,
    METRICS_RESPONSE_BYTES,
> = ubos_metrics::metrics::MetricsStatics::new();

/// Polls per phase. Wider than `run_thermal`'s, because a phase here
/// advances the clock by one `stale_timeout` and the loop then works off
/// every catch-up control tick in that window before it parks.
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

pub(crate) fn moderate_reading(no_reading: bool) -> ThermalReading {
    // Well below the default 67°C target: the PID drives the PWM toward
    // its floor rather than toward FULL, so a reading that carries a
    // measurement at this temperature unambiguously demonstrates the fan
    // leaving FULL.
    let temp = Temperature::from_millicelsius(50_000);
    ThermalReading {
        chain_voltage_mv: None,
        chip_max: Some(temp),
        pid_temp: (!no_reading).then_some(temp),
        ambient: None,
        timestamp_ms: 0,
    }
}

/// Polls `cooling` and `ingest` up to [`POLL_BUDGET`] times, enough for
/// the awaits triggered by the last clock advance or published reading
/// to settle. Both futures never complete — `run_cooling` is `-> !`,
/// `run_ingest` is `-> !` and the channel never empties on its own — so
/// the return value carries no information; only the side effects on
/// `crate::STATE_WATCH` matter.
/// Takes the pinned futures by reborrow so the caller's `pin!` bindings
/// stay usable across the repeated calls one per test phase.
pub(crate) fn drive<Cooling: Future, Ingest: Future>(
    mut cooling: Pin<&mut Cooling>,
    mut ingest: Pin<&mut Ingest>,
) {
    let mut cx = Context::from_waker(Waker::noop());
    for _ in 0..POLL_BUDGET {
        let _ = cooling.as_mut().poll(&mut cx);
        let _ = ingest.as_mut().poll(&mut cx);
    }
}
