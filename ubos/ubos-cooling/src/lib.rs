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
//! tachometer publishes its own labeled `board_fan_rpm` series.
//!
//! The duty comes from the PID under `FanMode::Auto` and from the
//! configured constant under `FanMode::Fixed`. The failsafes below are
//! not part of that choice: a reading with no measurement and a stalled
//! thermal task force the fan to full in either mode.

#![no_std]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::missing_panics_doc)]
#![allow(clippy::module_name_repetitions)]

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
use ubos_core::units::{FanPwm, FanRpm, Temperature};
use ubos_logging::{error, info};
use ubos_metrics::metrics::{Label, LabelValue, Labels, Metric, Value, push};
use ubos_thermal::Handle as ThermalHandle;

use crate::pid::TempControl;

type CSMutex = CriticalSectionRawMutex;

/// Board-supplied fan PWM sink. Boards translate the [`FanPwm`] duty
/// cycle to whatever counter top their PWM peripheral uses.
pub trait FanOutput {
    fn set_pwm(&mut self, pwm: FanPwm);
}

/// Board-supplied tachometer reader for `N` fans. `run_cooling` calls
/// `read_rpms` once at startup and once per [`CONTROL_INTERVAL`] tick, so the
/// measurement window is nominally a fixed interval rather than the variable
/// gap between loop wakes; a burst of thermal readings can delay a tach sample
/// past one tick, and drops it only if thermal publishes faster than the loop
/// services events, which the validated 100 ms sample-interval floor precludes.
/// The implementation returns the speed of each fan measured since the previous
/// call.
///
/// `LABELS[i]` is the `fan` label attached to fan `i`'s `board_fan_rpm`
/// series. `None` publishes that series with no label, which single-fan
/// boards use to keep the historical unlabeled metric.
///
/// `read_rpms` is async for the same reason
/// [`ubos_thermal::TemperatureSensor::sample`] is: a board whose tach lives
/// behind a bus has to await the transfer, and a synchronous signature would
/// force it to poll on a task of its own and park each sample in a static for
/// this loop to pick up. A board counting edges in hardware never awaits, so
/// the keyword costs it nothing.
///
/// An implementation that can block must bound itself. This loop has no
/// supervision of its own, but it commands the fan before it calls `read_rpms`,
/// so a read that never returns stalls the RPM and not the duty; the RPM is
/// telemetry and is worth no risk to the duty.
#[allow(async_fn_in_trait)]
pub trait TachReader<const N: usize> {
    const LABELS: [Option<&'static str>; N];
    async fn read_rpms(&mut self) -> [FanRpm; N];
}

const SECONDS_PER_MINUTE: u64 = 60;
const MICROS_PER_SECOND: u64 = 1_000_000;

/// Convert tachometer pulses counted over `elapsed_us` into RPM.
///
/// Every edge-counting [`TachReader`] ends in this formula: the pulses seen
/// in one measurement window, divided by the pulses one revolution emits,
/// scaled from the window length to a minute. A zero window reports 0 rather
/// than dividing by it, which is what the first read after construction may
/// see, and so does a zero `pulses_per_rev`, which no real fan has. A result
/// past `u32::MAX` saturates.
#[must_use]
pub fn pulses_to_rpm(pulses: u64, pulses_per_rev: u64, elapsed_us: u64) -> FanRpm {
    let rpm = (pulses * SECONDS_PER_MINUTE * MICROS_PER_SECOND)
        .checked_div(pulses_per_rev)
        .and_then(|per_rev| per_rev.checked_div(elapsed_us))
        .unwrap_or(0);
    FanRpm::from_rpm(u32::try_from(rpm).unwrap_or(u32::MAX))
}

const METRIC_FAN_PWM: &str = "board_fan_pwm_percent";
const METRIC_FAN_RPM: &str = "board_fan_rpm";

/// Subscriber capacity for the fan watch. Every reader takes a
/// point-in-time snapshot via [`Handle::current`]; none subscribe, so a
/// single slot suffices.
const WATCH_CAPACITY: usize = 1;

/// Fan telemetry published by [`run_cooling`], the sole writer.
///
/// [`Handle::current`] returns `None` until the first control tick, and
/// forever on boards that never spawn the cooling task; consumers render
/// the fan line only once a value is present.
#[derive(Clone, Copy)]
pub struct FanState {
    /// PWM duty the cooling loop last commanded.
    pub pwm: FanPwm,
    /// Fastest tachometer reading from the last control tick. The tach is
    /// sampled only on the control tick, never on a thermal-reading wake, so
    /// a value republished on a thermal wake repeats the previous tick's
    /// sample. The display shows one fan line; on multi-fan boards this is
    /// the busiest fan.
    pub rpm: FanRpm,
}

/// Module-internal fan-telemetry watch. Single writer is [`run_cooling`];
/// readers obtain a snapshot through [`Handle::current`].
static STATE_WATCH: Watch<CSMutex, FanState, WATCH_CAPACITY> = Watch::new();

/// Obtain a handle to the fan telemetry. `Copy`; callers freely pass it
/// to multiple tasks.
#[must_use]
pub fn handle() -> Handle {
    Handle {
        state: &STATE_WATCH,
    }
}

/// Consumer-side handle for fan telemetry.
///
/// `Copy` because it holds only a `'static` reference. Every consumer
/// task takes its own value.
#[derive(Clone, Copy)]
pub struct Handle {
    state: &'static Watch<CSMutex, FanState, WATCH_CAPACITY>,
}

impl Handle {
    /// Most recently published fan telemetry, or `None` before the first
    /// control tick and on boards without a cooling task.
    #[must_use]
    pub fn current(&self) -> Option<FanState> {
        self.state.try_get()
    }
}

/// Duty the fan holds under [`FanMode::Fixed`]. The configured percent
/// flows straight through, so a fixed 0 stops the fan: the floor in
/// [`ubos_config::PidConfig::min_fan_pwm`] bounds PID output only.
fn fixed_pwm(config: &CoolingConfig) -> FanPwm {
    FanPwm::from_percent(config.fixed_fan_pwm)
}

/// Fold a published cooling config into the controller.
///
/// A mode or gains change rebuilds it. Under new gains the accumulated
/// integral describes a loop that no longer exists, and a mode change
/// brackets a stretch of fixed duty the PID was not driving the fan through.
/// The mode test does not separate the two directions: only the return to
/// auto has stale integral to discard, but a controller nothing is reading
/// costs nothing to rebuild, so entering fixed mode rebuilds it too.
/// Anything else only moves the setpoint, which is idempotent, so it needs
/// no comparison of its own. The fixed duty needs no controller state at
/// all; the control tick in [`run_cooling`] is what carries it to the fan.
///
/// The config watch publishes on any config change, so an unchanged cooling
/// section returns before logging and leaves an unrelated update silent here.
fn refresh_control(control: &mut TempControl, old: &CoolingConfig, new: &CoolingConfig) {
    if new == old {
        return;
    }
    let rebuild = new.mode != old.mode || new.pid != old.pid;
    if rebuild {
        control.reconfigure(new);
    } else {
        control.set_target(new.target_temp);
    }
    log_cooling("cooling: updated", new);
}

/// Log the whole cooling state on one line. Printing the resulting state
/// rather than naming the field that moved keeps the call sites free of the
/// branching a per-field message would need, and answers the question an
/// operator reading the log actually has.
fn log_cooling(prefix: &str, config: &CoolingConfig) {
    info!(
        "{} (mode={}, target={}°C, floor={}%, fixed={}%)",
        prefix,
        config.mode.as_str(),
        config.target_temp.celsius_f64(),
        config.pid.min_fan_pwm,
        config.fixed_fan_pwm
    );
}

/// Cadence at which the cooling loop wakes independently of the thermal
/// subsystem: it reads the tachometer, republishes telemetry so the
/// display and metrics stay fresh while thermal is quiet, and enforces the
/// staleness failsafe below. Kept short so the failsafe is evaluated often;
/// the detection latency is dominated by `stale_timeout`, the larger of
/// `MIN_STALE_TIMEOUT` and a multiple of the thermal sample interval, and
/// this interval only bounds the extra delay before a lapsed `stale_timeout`
/// is noticed.
const CONTROL_INTERVAL: Duration = Duration::from_secs(1);

/// Multiplier on the thermal sample interval before the thermal subsystem
/// is judged stalled. A live thermal task goes silent while it tolerates
/// consecutive sensor misses on a board it still owes a reading to:
/// `ubos-thermal` withholds a reading until its miss count reaches
/// `SENSOR_FAULT_TICKS`, then publishes a reading with no PID
/// temperature on that tick and every one after; an idle board is a
/// separate case and publishes one on its first idle tick. The window must
/// therefore exceed `SENSOR_FAULT_TICKS` intervals, or ordinary sensor
/// hiccups that thermal itself is designed to ride out would trip this
/// failsafe first; the assertion below pins a minimum margin above
/// `SENSOR_FAULT_TICKS` and is the single source of truth for it.
const STALE_INTERVAL_MULTIPLIER: u32 = 7;

/// The staleness window must outlast the run of sensor misses `ubos-thermal`
/// rides out silently, or an ordinary sensor hiccup would trip this failsafe
/// before thermal itself reacts. Assert the design margin over
/// `SENSOR_FAULT_TICKS` at compile time, so a change to either constant that
/// narrows the margin fails the build rather than eroding it unnoticed.
const _: () = assert!(STALE_INTERVAL_MULTIPLIER >= ubos_thermal::SENSOR_FAULT_TICKS + 2);

/// Floor for the staleness window, so a fast thermal sample rate cannot
/// shrink it to where ordinary scheduling jitter trips a false failsafe.
const MIN_STALE_TIMEOUT: Duration = Duration::from_secs(3);

/// Window of thermal silence after which the subsystem is treated as
/// stalled and the fan forced to full. It is the larger of the multiplied
/// thermal sample interval and `MIN_STALE_TIMEOUT`, so a fast sample rate
/// floors it at `MIN_STALE_TIMEOUT` while a slow one scales it up; a wedged
/// thermal task runs at most this long plus one `CONTROL_INTERVAL` before the
/// failsafe trips, since the staleness check runs only on the control tick.
/// Across that window the fan is held at the configured `min_fan_pwm` floor,
/// or at the fixed duty in `FanMode::Fixed`, which the failsafe overrides with
/// full once the window elapses. A board that configures either to 0 therefore
/// gets no airflow until the window closes; the timeout, not the commanded
/// duty, is what bounds that exposure.
fn stale_timeout(thermal_sample_interval_ms: u32) -> Duration {
    let scaled = Duration::from_millis(
        u64::from(thermal_sample_interval_ms) * u64::from(STALE_INTERVAL_MULTIPLIER),
    );
    scaled.max(MIN_STALE_TIMEOUT)
}

/// Why the fan is held at full, when it is. A hold overrides the PID
/// and the fixed duty alike, so a low fixed duty costs hashrate rather
/// than the board.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hold {
    /// The duty follows the PID, or the fixed setting.
    None,
    /// The last thermal reading carried no measurement. Full until one
    /// does: the fan parks rather than following a value never
    /// measured, and the control tick does not hand a fixed duty or the
    /// floor back to the fan in between two such readings, which would
    /// cycle it.
    NoMeasurement,
    /// No thermal reading inside the stale window. The thermal task is
    /// wedged and the PID can no longer be trusted, so full until a
    /// reading arrives.
    Stalled,
}

/// What one step of the decision reports: the duty to command, and the
/// transition to log if this step made one.
#[derive(Debug, PartialEq, Eq)]
struct Step {
    pwm: FanPwm,
    note: Option<Note>,
}

/// A transition of [`Hold::Stalled`], which the loop logs once on entry
/// and once on exit.
#[derive(Debug, PartialEq, Eq)]
enum Note {
    /// The stale window elapsed with no thermal reading; carries the
    /// silence measured.
    Stalled(Duration),
    /// A thermal reading arrived after a stall, whether or not it
    /// carried a measurement: one without still clears the stall,
    /// because the task is alive to have sent it. The fan need not have
    /// left full.
    Recovered,
}

/// The fan decision, apart from the hardware and the clock. The loop
/// selects an event, hands it to the method the event names with the
/// time it happened, and acts on the [`Step`] that comes back. Nothing
/// here awaits, so every transition is a plain call a test can drive.
struct Cooling {
    config: CoolingConfig,
    control: TempControl,
    stale: Duration,
    hold: Hold,
    /// The duty last commanded. A control tick in auto raises it to the
    /// floor and otherwise leaves it, so it must survive between steps.
    pwm: FanPwm,
    last_reading: Instant,
}

impl Cooling {
    /// Start from `config` at `now`: the fan at the configured floor, or
    /// at the fixed duty when the operator has taken the fan off the PID.
    /// Either percent flows straight through `from_percent` with no
    /// special case, so a configured 0 yields a stopped fan by
    /// construction, held until the first reading.
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
        }
    }

    /// Fold a published config in: the cooling section into the
    /// controller, the thermal sample interval into the stale window.
    fn reconfigure(&mut self, updated: &Config) {
        refresh_control(&mut self.control, &self.config, &updated.cooling);
        self.config = updated.cooling.clone();
        self.stale = stale_timeout(updated.thermal.sample_interval_ms);
    }

    /// A thermal reading arrived at `now`, carrying `pid_temp` when it
    /// measured something. A fresh reading always carries a PID
    /// temperature and a held one never does, see
    /// `ThermalReading::pid_temp`, so the one field answers both whether
    /// this tick measured and what it measured.
    ///
    /// A run of readings with no measurement skips `control.update()`
    /// each time, so the smoother and ring buffer keep the last good
    /// sample across that run. The derivative term on the first reading
    /// that carries one is then computed against a pre-run value;
    /// acceptable because the fan parks at full on every reading of the
    /// run, so that stale derivative never drives the fan. The input
    /// smoother is the exception: the first measurement after any hold
    /// restarts it, because a stale input would drive the fan on every
    /// tick until the smoother caught up. This covers
    /// that run only; a thermal silence shorter than the stale window
    /// triggers no graduated response: the control tick merely resamples
    /// the tach and republishes telemetry until the window elapses.
    fn on_reading(&mut self, pid_temp: Option<Temperature>, now: Instant) -> Step {
        self.last_reading = now;
        let note = (self.hold == Hold::Stalled).then_some(Note::Recovered);
        self.pwm = match pid_temp {
            None => {
                self.hold = Hold::NoMeasurement;
                FanPwm::FULL
            }
            Some(pid_temp) => {
                if self.hold != Hold::None {
                    self.control.forget_input();
                }
                self.hold = Hold::None;
                match self.config.mode {
                    // Fixed ignores the reading and never advances the
                    // PID; the mode transition back to auto rebuilds the
                    // controller, so no state is carried across.
                    FanMode::Fixed => fixed_pwm(&self.config),
                    FanMode::Auto => FanPwm::from_percent(self.control.update(pid_temp)),
                }
            }
        };
        Step {
            pwm: self.pwm,
            note,
        }
    }

    /// The control tick fired at `now`, independent of the thermal task.
    /// If that task has gone silent past the stale window it is wedged
    /// and the PID can no longer be trusted, so fail safe to full.
    ///
    /// Otherwise fixed mode has no other cadence: thermal readings do
    /// not change the duty, so this tick is what carries a new fixed
    /// duty to the fan, within one `CONTROL_INTERVAL` of the config
    /// update. Auto raises the fan to the floor instead, which is what
    /// the transition out of fixed needs: the PID commands nothing until
    /// the next thermal reading, so a fixed duty below the floor, 0
    /// included, would otherwise hold the fan for a whole sample
    /// interval after the operator left fixed mode. The PID clamps its
    /// own output to that same floor, so this only bites inside that
    /// window, and it raises the duty rather than lowering it.
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

/// Publish the [`FanState`] plus per-fan metrics for the given tachometer
/// readings. The caller commands the fan itself and samples the tachometer on
/// the control tick, then passes the most recent readings here, so a
/// thermal-driven wake republishes fresh PWM with the last sampled RPM rather
/// than re-reading the counter over a short, uneven window.
async fn publish_state<const N: usize>(
    pwm: FanPwm,
    rpms: &[FanRpm; N],
    tach_labels: &[Option<&'static str>; N],
) {
    let fastest = rpms
        .iter()
        .copied()
        .max_by_key(|reading| reading.rpm())
        .unwrap_or(FanRpm::from_rpm(0));
    STATE_WATCH.sender().send(FanState { pwm, rpm: fastest });

    push(Metric::with_no_labels(
        METRIC_FAN_PWM,
        Value::Uint(u64::from(pwm.percent())),
    ))
    .await;
    for (label, reading) in tach_labels.iter().zip(rpms.iter()) {
        let value = Value::Uint(u64::from(reading.rpm()));
        let metric = match *label {
            None => Metric::with_no_labels(METRIC_FAN_RPM, value),
            Some(name) => {
                let mut labels = Labels::new();
                let _ = labels.push(Label {
                    name: "fan",
                    value: LabelValue::Static(name),
                });
                Metric::with_labels(METRIC_FAN_RPM, labels, value)
            }
        };
        push(metric).await;
    }
}

/// Run the cooling loop forever.
///
/// The loop runs on its own [`CONTROL_INTERVAL`] tick and treats the
/// thermal subsystem as an input that may go stale rather than the clock
/// that drives it: each iteration `select`s between the next thermal
/// reading and the control tick. A fresh reading advances the PID, or
/// forces full fan when it carries no PID temperature, and resets the
/// staleness clock; a
/// control tick that finds no reading for [`stale_timeout`] forces the fan
/// to 100%, so a thermal task wedged in a hung I2C read can never
/// leave the board uncooled. Either wake republishes telemetry, and in
/// auto the fan starts at the configured floor, held at `min_fan_pwm`
/// until the first reading, so the display shows fan state from boot
/// rather than a blank line.
///
/// Under [`FanMode::Fixed`] the thermal reading no longer sets the duty:
/// the fan holds [`CoolingConfig::fixed_fan_pwm`], refreshed on each control
/// tick so a duty change lands within one [`CONTROL_INTERVAL`] whatever the
/// thermal cadence. Both failsafes still apply — a reading with no PID
/// temperature and a lapsed staleness window force full in either mode —
/// and the mining halt on the thermal config's dangerous thresholds is
/// unaffected, so a low fixed duty costs hashrate rather than the board.
///
/// Cooling tuning is read from `config` and refreshed between samples: a
/// target-only change adjusts the setpoint in place, a PID-gains or mode
/// change rebuilds the controller. Board firmware passes
/// [`ubos_config::handle`], which [`cooling_task`] supplies for it. This
/// task is the sole writer of the fan watch.
pub async fn run_cooling<F, T, const N: usize>(
    mut fan: F,
    mut tach: T,
    thermal_handle: ThermalHandle,
    config_handle: ConfigHandle,
) -> !
where
    F: FanOutput,
    T: TachReader<N>,
{
    let mut config_subscription = config_handle.subscribe();
    let mut cooling = Cooling::new(&config_handle.current(), Instant::now());

    let tach_labels = T::LABELS;

    // Command the starting duty and publish immediately so the display
    // shows fan state from boot.
    fan.set_pwm(cooling.pwm);

    // Sample the tach once up front so boot telemetry is not a blank line; the
    // control tick refreshes it every CONTROL_INTERVAL thereafter.
    let mut last_rpms = tach.read_rpms().await;
    publish_state(cooling.pwm, &last_rpms, &tach_labels).await;
    log_cooling("cooling: started", &cooling.config);

    let mut subscription = thermal_handle.subscribe();
    let mut ticker = Ticker::every(CONTROL_INTERVAL);
    loop {
        let event = select(subscription.changed(), ticker.next()).await;

        if let Some(updated) = config_subscription.try_changed() {
            cooling.reconfigure(&updated);
        }

        let step = match &event {
            Either::First(reading) => cooling.on_reading(reading.pid_temp, Instant::now()),
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
        // Command the fan before the tach read: read_rpms can await a bus
        // transfer that never completes, and this tick's duty, the
        // staleness failsafe included, must not wait on it.
        fan.set_pwm(step.pwm);
        // Sample the tach once per control tick, so the edge count has a
        // nominally fixed CONTROL_INTERVAL window independent of the
        // thermal-reading cadence: delayed, not skipped, if thermal
        // readings monopolize the select. A thermal wake republishes the
        // fresh duty with the last sampled RPM.
        if let Either::Second(()) = event {
            last_rpms = tach.read_rpms().await;
        }
        publish_state(step.pwm, &last_rpms, &tach_labels).await;
    }
}

/// Task shell around [`run_cooling`], the fan PID loop. Takes the
/// board's fan output and tachometer reader types.
#[macro_export]
macro_rules! cooling_task {
    ($fan:ty, $tach:ty) => {
        #[embassy_executor::task]
        async fn cooling_task(fan: $fan, tach: $tach, thermal_handle: ubos_thermal::Handle) -> ! {
            ubos_cooling::run_cooling(fan, tach, thermal_handle, ubos_config::handle()).await
        }
    };
}
