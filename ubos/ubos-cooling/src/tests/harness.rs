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
//! commanded PWM is asserted through the fan handle.
//!
//! A reading comes from the real `run_thermal` over a sensor that submits
//! it once, and a config change from the real config task over a file store,
//! so the loop is driven by what the firmware runs. Each test takes the
//! handles as a board does and runs in a process of its own.

use core::cell::Cell;
use core::future::Future;
use core::pin::{Pin, pin};
use core::task::{Context, Waker};

use std::format;

use embassy_time::{Duration, MockDriver};
use ubos_config::{
    Config, DefaultTuning, FileStorage, Handle as ConfigHandle, Message, MiningBounds, MiningInfo,
    Runner as ConfigRunner, TunerInfo,
};
use ubos_core::index::HashboardIndex;
use ubos_core::units::{FanPwm, FanRpm, Temperature};
use ubos_manager::Handle as ManagerHandle;
use ubos_temperature::{Reading as TempReading, TempSource};
use ubos_thermal::{
    Handle as ThermalHandle, NoAmbient, NoMcu, Reading, Runner as ThermalRunner, TempSample,
    TempStats, TemperatureSensor, run_thermal,
};

use crate::{FanOutput, Handle, Runner, TachReader};

/// The handles a board takes in `main`. They are taken once, so each test
/// runs in a process of its own.
pub(crate) struct Rig {
    pub(crate) fan: Handle,
    pub(crate) thermal: ThermalHandle,
    /// Nothing runs the manager.
    pub(crate) manager: ManagerHandle,
    pub(crate) config: ConfigHandle,
    dir: std::path::PathBuf,
}

/// Apart from [`Rig`], because each task borrows its runner mutably.
pub(crate) struct Runners {
    pub(crate) fan: Runner,
    pub(crate) thermal: ThermalRunner,
    pub(crate) config: ConfigRunner,
}

impl Rig {
    pub(crate) fn take() -> (Self, Runners) {
        let dir = std::env::temp_dir().join(format!(
            "ubos_cooling_{}_{:x}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |duration| duration.as_nanos()),
        ));
        let (fan_runner, fan) = Handle::take();
        let (thermal_runner, thermal) = ThermalHandle::take();
        let (_manager_runner, manager) = ManagerHandle::take();
        let (config_runner, config) = ConfigHandle::take();
        let rig = Self {
            fan,
            thermal,
            manager,
            config,
            dir,
        };
        let runners = Runners {
            fan: fan_runner,
            thermal: thermal_runner,
            config: config_runner,
        };
        (rig, runners)
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A sensor that answers once. It submits the reading it was given, as a
/// source does, so the temperature that cooling reads comes through the
/// filter of thermal. With no reading it answers as a board that has nothing to
/// measure.
struct OneReading {
    reading: Option<TempReading>,
    thermal: ThermalHandle,
    answered: bool,
}

impl TemperatureSensor for OneReading {
    async fn sample(&mut self) -> Reading {
        assert!(
            !self.answered,
            "BUG: test drove run_thermal past its one reading"
        );
        self.answered = true;
        let Some(reading) = self.reading else {
            return Reading::Idle;
        };
        self.thermal.submit(reading).await;
        let stats = TempStats {
            max: reading.value,
            mean: reading.value,
        };
        Reading::Measured(TempSample {
            chip: None,
            hashboard: Some(stats),
        })
    }
}

/// The hashboard whose defaults the config task and the tests run on.
const BOARD: &ubos_asic::HashboardDescriptor = &ubos_asic::BMM101_HASHBOARD;

/// The bounds of a machine that offers the fixed tuner alone.
static BOUNDS: MiningBounds = BOARD.bounds(&[TunerInfo::Fixed { name: "Fixed" }], None);

/// The mining metadata the config task runs on.
static INFO: MiningInfo = BOARD.mining_info(&BOUNDS, DefaultTuning::Fixed);

/// The config that the config task publishes over an empty store.
pub(crate) fn default_config() -> Config {
    Config::for_board(&BOARD.defaults, DefaultTuning::Fixed)
}

/// The real config task over a store in a directory of its own.
pub(crate) fn run_config<'a>(
    rig: &Rig,
    runner: &'a mut ConfigRunner,
) -> impl Future<Output = core::convert::Infallible> + use<'a> {
    let dir = rig.dir.clone();
    async move {
        ubos_config::run(
            runner,
            FileStorage::new(dir),
            &BOARD.defaults,
            &INFO,
            None,
            None,
        )
        .await
    }
}

/// Series slots in the map the ingest task fills. The tests read no
/// exposition, so any power of two serves.
const MAX_METRICS: usize = 64;

/// Bytes of the exposition buffer, which no test renders into.
const METRICS_RESPONSE_BYTES: usize = 4096;

/// Each test has a process of its own, so it has a store of its own.
pub(crate) static METRICS_STATICS: ubos_metrics::metrics::MetricsStatics<
    MAX_METRICS,
    METRICS_RESPONSE_BYTES,
> = ubos_metrics::metrics::MetricsStatics::new();

/// Polls per phase. Wider than `run_thermal`'s, because a phase here
/// advances the clock by one `stale_timeout` and the loop then works off
/// every catch-up control tick in that window before it parks.
pub(crate) const POLL_BUDGET: usize = 64;

/// Polls `future` far enough for the awaits one phase triggers to settle.
pub(crate) fn settle<F: Future>(future: &mut Pin<&mut F>) {
    let mut cx = Context::from_waker(Waker::noop());
    for _ in 0..POLL_BUDGET {
        let _ = future.as_mut().poll(&mut cx);
    }
}

/// Publish `reading` through a `run_thermal` that starts now, sees one
/// sample interval pass on the mock clock and samples once.
///
/// A new task per reading owes no ticks for a long silence before it.
/// `rest` polls the tasks that react, after the clock has moved.
pub(crate) fn publish_reading(
    rig: &Rig,
    runner: &mut ThermalRunner,
    driver: &MockDriver,
    reading: Option<TempReading>,
    mut rest: impl FnMut(),
) {
    let interval = u64::from(
        embassy_futures::block_on(rig.config.current())
            .thermal
            .sample_interval_ms,
    );
    let mut thermal = pin!(run_thermal(
        OneReading {
            reading,
            thermal: rig.thermal,
            answered: false,
        },
        NoAmbient,
        NoMcu,
        runner,
        rig.manager,
        rig.config
    ));
    settle(&mut thermal);
    driver.advance(Duration::from_millis(interval));
    let mut cx = Context::from_waker(Waker::noop());
    for _ in 0..POLL_BUDGET {
        let _ = thermal.as_mut().poll(&mut cx);
        rest();
    }
}

/// Send `config` to the config task, as the API does, and let the tasks
/// react. `rest` polls the config task and the tasks that read it.
pub(crate) fn publish_config(rig: &Rig, config: Config, mut rest: impl FnMut()) {
    {
        let mut send = pin!(rig.config.send(Message::Update(config)));
        let _ = send.as_mut().poll(&mut Context::from_waker(Waker::noop()));
    }
    rest();
}

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
    async fn set_pwm(&mut self, pwm: FanPwm) {
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

/// The I2C address of the sensor that the readings of the tests name.
const SENSOR: u8 = 0x48;

/// A hashboard reading well below the default 67°C target, so the PID
/// drives the PWM toward its floor and not toward FULL. With `no_reading`
/// it is the answer of a board that has nothing to measure.
pub(crate) fn moderate_reading(no_reading: bool) -> Option<TempReading> {
    let reading = TempReading {
        source: TempSource::hashboard(HashboardIndex(0), SENSOR),
        value: Temperature::from_millicelsius(50_000),
    };
    (!no_reading).then_some(reading)
}

/// Polls each pinned future [`POLL_BUDGET`] times, enough for the awaits
/// the last clock advance or published reading triggered to settle. Each
/// task is `-> !`, so only its side effects on the handles matter.
macro_rules! drive {
    ($($future:expr),+ $(,)?) => {{
        let mut cx = core::task::Context::from_waker(core::task::Waker::noop());
        for _ in 0..$crate::tests_harness::POLL_BUDGET {
            $( let _ = $future.as_mut().poll(&mut cx); )+
        }
    }};
}
pub(crate) use drive;
