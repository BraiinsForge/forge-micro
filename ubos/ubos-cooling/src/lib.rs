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

//! Fan controller task: subscribes to the thermal subsystem, drives
//! the board fan via the `FanOutput` trait, reads the tachometers via
//! `TachReader`, and publishes the resulting `FanState` as the sole
//! writer. Generic over the thermal source, fan output, and tach reader
//! to stay portable across boards. A board may carry several fans; each
//! tachometer publishes its own labeled `fan_rpm` series. A board
//! whose fans sit behind a fan controller chip takes its driver from
//! here: [`emc2302_1`] drives the EMC2302-1.
//!
//! The duty comes from the PID under `FanMode::Auto` and from the
//! configured constant under `FanMode::Fixed`. The failsafes below are
//! not part of that choice: a reading with no measurement and a stalled
//! thermal task force the fan to full in either mode.

#![no_std]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::missing_panics_doc)]
#![allow(clippy::module_name_repetitions)]

// The host test binary links std for the config store, which writes files,
// and for the `rusty-fork` macro.
#[cfg(test)]
extern crate std;

pub mod emc2302_1;
mod pid;

#[cfg(test)]
#[path = "tests/harness.rs"]
mod tests_harness;

#[cfg(test)]
#[path = "tests/pid.rs"]
mod tests_pid;

#[cfg(test)]
#[path = "tests/stale_timeout.rs"]
mod tests_stale_timeout;

#[cfg(test)]
#[path = "tests/failsafe.rs"]
mod tests_failsafe;

#[cfg(test)]
#[path = "tests/fixed.rs"]
mod tests_fixed;

#[cfg(test)]
#[path = "tests/rpm.rs"]
mod tests_rpm;

#[cfg(test)]
#[path = "tests/decide.rs"]
mod tests_decide;

use embassy_futures::select::{Either, select};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::watch::Watch;
use embassy_time::{Duration, Instant, Ticker};
use ubos_config::Handle as ConfigHandle;
use ubos_config::{Config, CoolingConfig, FanMode};
use ubos_config::{PidTarget, Source, Statistic};
use ubos_core::units::{FanPwm, FanRpm, Temperature};
use ubos_logging::{error, info};
use ubos_metrics::catalog::{STAT_LABEL, TYPE_LABEL};
use ubos_metrics::metric;
use ubos_metrics::value::Flag;
use ubos_thermal::Handle as ThermalHandle;
use ubos_thermal::PidTemperature;

use crate::pid::TempControl;

type CSMutex = CriticalSectionRawMutex;

/// Board-supplied fan PWM sink. `set_pwm` is async for the reason
/// [`TachReader::read_rpms`] is; unlike the tach read, the write carries
/// the duty itself, so one that never returns stalls [`run_cooling`] with
/// its failsafes.
#[allow(async_fn_in_trait)]
pub trait FanOutput {
    async fn set_pwm(&mut self, pwm: FanPwm);
}

/// Board-supplied tachometer reader for `N` fans, returning each fan's
/// speed since the previous call. Called once per `CONTROL_INTERVAL`,
/// so the measurement window is nominally that interval rather than the
/// variable gap between loop wakes.
///
/// `LABELS[i]` is fan `i`'s `fan` label; `None` keeps the unlabelled
/// series a single-fan board has always published.
///
/// Async so a tach behind a bus can await its transfer. An
/// implementation that can block must bound itself, though the loop
/// commands the fan before it reads, so a read that never returns stalls
/// the telemetry and not the duty.
#[allow(async_fn_in_trait)]
pub trait TachReader<const N: usize> {
    const LABELS: [Option<&'static str>; N];
    async fn read_rpms(&mut self) -> [FanRpm; N];
}

const SECONDS_PER_MINUTE: u64 = 60;
const MICROS_PER_SECOND: u64 = 1_000_000;

/// Pulses counted over `elapsed_us` as RPM. A zero window, which the
/// first read can see, and a zero `pulses_per_rev` both report 0 rather
/// than dividing; a result past `u32::MAX` saturates.
#[must_use]
pub fn pulses_to_rpm(pulses: u64, pulses_per_rev: u64, elapsed_us: u64) -> FanRpm {
    let rpm = (pulses * SECONDS_PER_MINUTE * MICROS_PER_SECOND)
        .checked_div(pulses_per_rev)
        .and_then(|per_rev| per_rev.checked_div(elapsed_us))
        .unwrap_or(0);
    FanRpm::from_rpm(u32::try_from(rpm).unwrap_or(u32::MAX))
}

/// Every reader takes a snapshot through [`Handle::current`]; none
/// subscribe.
const WATCH_CAPACITY: usize = 1;

/// Fan telemetry; [`run_cooling`] is the sole writer.
#[derive(Clone, Copy)]
pub struct FanState {
    pub pwm: FanPwm,
    /// The fastest fan at the last control tick, which is the only wake
    /// that samples the tach, so a thermal wake republishes that sample.
    pub rpm: FanRpm,
    /// The kind of reading the PID follows. `None` under
    /// [`FanMode::Fixed`] and while a failsafe holds the fan at full.
    pub pid_source: Option<Source>,
}

type FanWatch = Watch<CSMutex, FanState, WATCH_CAPACITY>;

static STATE_WATCH: FanWatch = Watch::new();

static TAKEN: ubos_core::take_once::TakeOnce = ubos_core::take_once::TakeOnce::new("cooling");

/// Consumer-side handle. It holds no data: a firmware takes it once and
/// passes a copy to each task that uses it.
#[derive(Clone, Copy)]
pub struct Handle(());

/// The right to run the task that drives the fans.
///
/// It is not `Copy`, so a firmware that gives it to one task cannot give
/// it to a second.
pub struct Runner(());

#[allow(clippy::unused_self)]
impl Runner {
    pub(crate) fn publish(&self, state: FanState) {
        STATE_WATCH.sender().send(state);
    }
}

impl Handle {
    /// The firmware's one fan telemetry handle, and the runner of the task that
    /// drives the fans. A second call panics.
    #[must_use]
    #[track_caller]
    pub fn take() -> (Runner, Self) {
        TAKEN.take();
        (Runner(()), Self(()))
    }

    /// `None` before the first control tick and on a board with no
    /// cooling task.
    #[must_use]
    pub fn current(&self) -> Option<FanState> {
        STATE_WATCH.try_get()
    }
}

/// The configured percent flows straight through, so a fixed 0 stops
/// the fan: [`ubos_config::PidConfig::min_fan_pwm`] bounds PID output
/// only.
fn fixed_pwm(config: &CoolingConfig) -> FanPwm {
    FanPwm::from_percent(config.fixed_fan_pwm)
}

/// Fold a published cooling config into the controller. A mode or gains
/// change rebuilds it, since the accumulated integral describes a loop
/// that no longer exists; anything else only moves the setpoint, which
/// is idempotent. An unchanged cooling section returns before logging,
/// so an unrelated config update is silent here.
fn refresh_control(control: &mut TempControl, old: &CoolingConfig, new: &CoolingConfig) {
    if new == old {
        return;
    }
    let rebuild = new.mode != old.mode || new.pid != old.pid;
    if rebuild {
        control.reconfigure(new);
    } else {
        control.set_target(new.target.temp);
    }
    log_cooling("cooling: updated", new);
}

/// Log the whole resulting state, not the field that moved: it is what
/// an operator reading the log wants, and it needs no branching.
fn log_cooling(prefix: &str, config: &CoolingConfig) {
    info!(
        "{} (mode={}, target={} {} {}°C, floor={}%, fixed={}%)",
        prefix,
        config.mode.as_str(),
        config.target.source.as_str(),
        config.target.statistic.as_str(),
        config.target.temp.celsius_f64(),
        config.pid.min_fan_pwm,
        config.fixed_fan_pwm
    );
}

/// Cadence at which the loop wakes independently of the thermal task:
/// it reads the tach, republishes telemetry and checks the staleness
/// failsafe. Short, so it adds little to the detection latency that
/// [`stale_timeout`] dominates.
const CONTROL_INTERVAL: Duration = Duration::from_secs(1);

/// Multiplier on the thermal sample interval before the thermal subsystem is
/// judged stalled. A live thermal task publishes on every tick, so the window
/// only has to outlast a late tick, and a tick is late by as long as its
/// sample runs: a sensor array waits out a read timeout for each sensor that
/// does not answer. A false stall runs the fan at full.
const STALE_INTERVAL_MULTIPLIER: u32 = 7;

/// Floor for the staleness window, so a fast sample rate cannot shrink it into
/// scheduling jitter.
const MIN_STALE_TIMEOUT: Duration = Duration::from_secs(3);

/// Thermal silence after which the task is stalled and the fan forced
/// to full: the larger of the multiplied sample interval and
/// [`MIN_STALE_TIMEOUT`]. A wedged task runs at most this plus one
/// [`CONTROL_INTERVAL`], since the check runs on the control tick. Across
/// that window the fan holds the configured floor or fixed duty, so a
/// board that configures either to 0 gets no airflow until it closes.
fn stale_timeout(thermal_sample_interval_ms: u32) -> Duration {
    let scaled = Duration::from_millis(
        u64::from(thermal_sample_interval_ms) * u64::from(STALE_INTERVAL_MULTIPLIER),
    );
    scaled.max(MIN_STALE_TIMEOUT)
}

/// Why the fan is held at full. A hold overrides the PID and the fixed
/// duty alike, so a low fixed duty costs hashrate, not the board.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hold {
    None,
    /// The last reading carried no measurement, so the fan parks at full
    /// rather than following a value never measured. The control tick
    /// hands nothing back in between, which would cycle it.
    NoMeasurement,
    /// No reading inside the stale window, so the PID can no longer be
    /// trusted.
    Stalled,
}

#[derive(Debug, PartialEq, Eq)]
struct Step {
    pwm: FanPwm,
    note: Option<Note>,
}

/// A [`Hold::Stalled`] transition, logged once each way.
#[derive(Debug, PartialEq, Eq)]
enum Note {
    /// Carries the silence measured.
    Stalled(Duration),
    /// A reading arrived after a stall. One carrying no measurement
    /// still clears it: the task is alive to have sent it.
    Recovered,
}

/// What the PID follows on a reading: a statistic of one kind of
/// reading, its value, and the setpoint it is held at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PidInput {
    source: Source,
    statistic: Statistic,
    temp: Temperature,
    target: Temperature,
}

/// The fan decision, apart from the hardware and the clock. Nothing
/// here awaits, so every transition is a plain call a test can drive.
struct Cooling {
    config: CoolingConfig,
    control: TempControl,
    stale: Duration,
    hold: Hold,
    /// Survives between steps: a control tick in auto raises it to the
    /// floor and otherwise leaves it.
    pwm: FanPwm,
    last_reading: Instant,
    /// What the PID followed on the last measurement.
    pid_input: Option<PidInput>,
}

impl Cooling {
    /// Start at the configured floor, or at the fixed duty. Either
    /// percent flows straight through, so a configured 0 holds the fan
    /// stopped until the first reading.
    fn new(config: &Config, now: Instant) -> Self {
        let cooling = config.cooling.clone();
        let pwm = match cooling.mode {
            FanMode::Auto => FanPwm::from_percent(cooling.pid.min_fan_pwm),
            FanMode::Fixed => fixed_pwm(&cooling),
        };
        Self {
            control: TempControl::from_config(&cooling),
            config: cooling,
            stale: stale_timeout(config.thermal.sample_interval_ms),
            hold: Hold::None,
            pwm,
            last_reading: now,
            pid_input: None,
        }
    }

    /// What the PID follows, for the metrics. `None` under
    /// [`FanMode::Fixed`] and while a failsafe holds the fan at full.
    fn pid_input(&self) -> Option<PidInput> {
        let driving = self.config.mode == FanMode::Auto && self.hold == Hold::None;
        self.pid_input.filter(|_| driving)
    }

    fn reconfigure(&mut self, updated: &Config) {
        refresh_control(&mut self.control, &self.config, &updated.cooling);
        // The first reading of another kind would reach the input smoother
        // as a jump, so the smoother starts afresh, as after a hold.
        if updated.cooling.target.source != self.config.target.source {
            self.control.forget_input();
        }
        self.config = updated.cooling.clone();
        self.stale = stale_timeout(updated.thermal.sample_interval_ms);
    }

    /// The temperature the PID follows arrived from thermal at `now`. `None`
    /// means no reading of the target's kind is inside the window of the
    /// thermal filter, and it holds a fixed duty at full as it holds the PID.
    ///
    /// The reading names the kind and the statistic thermal selected it by.
    /// Thermal and this loop take a config change on their own schedules, so a
    /// reading can answer a target this loop has already left. The duty then
    /// stands until the next reading, because a value of another kind would
    /// reach the PID as its input.
    ///
    /// A run of readings without a measurement skips the PID update, so the
    /// first reading after the run computes its derivative against a pre-run
    /// value. That is safe because the fan parks at full across the whole run,
    /// so the stale derivative never drives it. The input smoother restarts
    /// after any hold, because a stale input would drive the fan on every tick
    /// until the smoother caught up.
    fn on_reading(&mut self, reading: PidTemperature, now: Instant) -> Step {
        self.last_reading = now;
        let note = (self.hold == Hold::Stalled).then_some(Note::Recovered);
        let PidTarget {
            source,
            statistic,
            temp: target,
        } = self.config.target;
        if (reading.source, reading.statistic) != (source, statistic) {
            // The stall ends on this reading as on any other, and the
            // fan stays at full until a reading steers it, so the
            // recovery is reported once.
            if self.hold == Hold::Stalled {
                self.pwm = self.hold_full();
            }
            return Step {
                pwm: self.pwm,
                note,
            };
        }
        self.pwm = match (self.config.mode, reading.temp) {
            (_, None) => self.hold_full(),
            // Fixed ignores the reading and never advances the PID; the
            // mode transition back to auto rebuilds the controller, so no
            // state is carried across.
            (FanMode::Fixed, Some(_)) => {
                self.leave_hold();
                fixed_pwm(&self.config)
            }
            (FanMode::Auto, Some(temp)) => {
                self.leave_hold();
                let pid_input = PidInput {
                    source,
                    statistic,
                    temp,
                    target,
                };
                self.pid_input = Some(pid_input);
                self.control.update(pid_input.temp)
            }
        };
        Step {
            pwm: self.pwm,
            note,
        }
    }

    /// Hold the fan at full on a reading that left nothing to steer by.
    fn hold_full(&mut self) -> FanPwm {
        self.hold = Hold::NoMeasurement;
        FanPwm::FULL
    }

    /// Leave a hold on a reading that measured, restarting the input
    /// smoother, because a stale input would drive the fan until the
    /// smoother caught up.
    fn leave_hold(&mut self) {
        if self.hold != Hold::None {
            self.control.forget_input();
        }
        self.hold = Hold::None;
    }

    /// The control tick, independent of the thermal task: full when that
    /// task has gone silent past the stale window.
    ///
    /// Otherwise this tick is fixed mode's only cadence, so a new fixed
    /// duty reaches the fan within one [`CONTROL_INTERVAL`]. In auto it
    /// raises the fan to the floor instead, which is what the transition
    /// out of fixed needs: the PID commands nothing until the next
    /// reading, so a low fixed duty would otherwise hold for a whole
    /// sample interval after the operator left fixed mode.
    fn on_tick(&mut self, now: Instant) -> Step {
        let silence = now.duration_since(self.last_reading);
        if silence >= self.stale {
            let note = (self.hold != Hold::Stalled).then_some(Note::Stalled(silence));
            self.hold = Hold::Stalled;
            self.pwm = FanPwm::FULL;
            return Step {
                pwm: self.pwm,
                note,
            };
        }
        if self.hold == Hold::None {
            self.pwm = match self.config.mode {
                FanMode::Fixed => fixed_pwm(&self.config),
                FanMode::Auto => self
                    .pwm
                    .max(FanPwm::from_percent(self.config.pid.min_fan_pwm)),
            };
        }
        Step {
            pwm: self.pwm,
            note: None,
        }
    }
}

/// Publish the [`FanState`] and the per-fan metrics. The caller passes
/// the most recent tach readings, so a thermal wake republishes fresh
/// PWM with the last sample instead of reading the counter over a short,
/// uneven window.
async fn publish_state<const N: usize>(
    runner: &Runner,
    pwm: FanPwm,
    pid: Option<PidInput>,
    rpms: &[FanRpm; N],
    tach_labels: &[Option<&'static str>; N],
) {
    let fastest = rpms
        .iter()
        .copied()
        .max_by_key(|reading| reading.rpm())
        .unwrap_or(FanRpm::from_rpm(0));
    runner.publish(FanState {
        pwm,
        rpm: fastest,
        pid_source: pid.map(|input| input.source),
    });

    metric!("fan_speed_control", pwm).push().await;
    for (label, reading) in tach_labels.iter().zip(rpms.iter()) {
        let metric = metric!("fan_rpm", *reading);
        let metric = match *label {
            None => metric,
            Some(name) => metric.label("fan", name),
        };
        metric.push().await;
    }
    publish_pid(pid).await;
}

/// Push what the PID follows: its value and its setpoint while it runs,
/// as the `type="measured"` and `type="target"` series of one gauge, and
/// a `pid_source` series for each kind and statistic, 1 on the one
/// it follows. Every `pid_source` series is pushed on each step,
/// so a pair the PID stopped following reads 0, and all four read 0
/// while the PID does not run, as `miner_state` marks the miner's
/// state.
async fn publish_pid(pid: Option<PidInput>) {
    if let Some(input) = pid {
        for (role, temp) in [("measured", input.temp), ("target", input.target)] {
            metric!("pid_temperature_celsius", temp)
                .label(TYPE_LABEL, role)
                .push()
                .await;
        }
    }
    for source in [Source::Chip, Source::Hashboard] {
        for statistic in [Statistic::Max, Statistic::Mean] {
            let followed =
                pid.is_some_and(|input| (input.source, input.statistic) == (source, statistic));
            metric!("pid_source", Flag(followed))
                .label("source", source.as_str())
                .label(STAT_LABEL, statistic.as_str())
                .push()
                .await;
        }
    }
}

/// Run the cooling loop forever.
///
/// The loop runs on its own `CONTROL_INTERVAL` tick and treats the
/// thermal task as an input that may go stale rather than the clock that
/// drives it, selecting between the next reading and the tick. That is
/// what keeps a thermal task wedged in a hung bus read from leaving the
/// board uncooled: see `Hold` for the two failsafes, which apply in
/// both fan modes.
pub async fn run_cooling<F, T, const N: usize>(
    mut fan: F,
    mut tach: T,
    runner: &mut Runner,
    thermal_handle: ThermalHandle,
    config_handle: ConfigHandle,
) -> !
where
    F: FanOutput,
    T: TachReader<N>,
{
    let mut config_subscription = config_handle.subscribe();
    let mut cooling = Cooling::new(&config_handle.current().await, Instant::now());

    let tach_labels = T::LABELS;

    // Published at once, so the display shows fan state from boot.
    fan.set_pwm(cooling.pwm).await;

    let mut last_rpms = tach.read_rpms().await;
    publish_state(runner, cooling.pwm, None, &last_rpms, &tach_labels).await;
    log_cooling("cooling: started", &cooling.config);

    let mut subscription = thermal_handle.subscribe_pid_temperature();
    let mut ticker = Ticker::every(CONTROL_INTERVAL);
    loop {
        let event = select(subscription.changed(), ticker.next()).await;

        if let Some(updated) = config_subscription.try_changed() {
            cooling.reconfigure(&updated);
        }

        let step = match &event {
            Either::First(reading) => cooling.on_reading(*reading, Instant::now()),
            Either::Second(()) => cooling.on_tick(Instant::now()),
        };
        match step.note {
            Some(Note::Stalled(silence)) => error!(
                "cooling: no thermal reading for {} ms; forcing fan to 100%",
                silence.as_millis()
            ),
            Some(Note::Recovered) => info!("cooling: thermal task recovered after stall"),
            None => {}
        }
        // The fan is commanded first: `read_rpms` can await a transfer
        // that never completes, and the duty must not wait on it.
        // TODO: Is it safe to await this write with no timeout? Currently
        // there is no continuous watchdog.
        fan.set_pwm(step.pwm).await;
        // Once per control tick, so the edge count has a fixed window;
        // see `TachReader`.
        if let Either::Second(()) = event {
            last_rpms = tach.read_rpms().await;
        }
        publish_state(
            runner,
            step.pwm,
            cooling.pid_input(),
            &last_rpms,
            &tach_labels,
        )
        .await;
    }
}

/// Task shell around [`run_cooling`].
#[macro_export]
macro_rules! cooling_task {
    ($fan:ty, $tach:ty) => {
        #[embassy_executor::task]
        async fn cooling_task(
            fan: $fan,
            tach: $tach,
            mut runner: ubos_cooling::Runner,
            thermal_handle: ubos_thermal::Handle,
            config_handle: ubos_config::Handle,
        ) -> ! {
            ubos_cooling::run_cooling(fan, tach, &mut runner, thermal_handle, config_handle).await
        }
    };
}
