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

//! Fan PID controller task: subscribes to the thermal subsystem, drives
//! the board fan via the `FanOutput` trait, reads the tachometers via
//! `TachReader`, and publishes the resulting `FanState` as the sole
//! writer. Generic over the thermal source, fan output, and tach reader
//! to stay portable across boards. A board may carry several fans; each
//! tachometer publishes its own labeled `board_fan_rpm` series.

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

use embassy_futures::select::{Either, select};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::watch::Watch;
use embassy_time::{Duration, Instant, Ticker};
use ubos_config::CoolingConfig;
use ubos_core::units::{FanPwm, FanRpm};
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
pub trait TachReader<const N: usize> {
    const LABELS: [Option<&'static str>; N];
    fn read_rpms(&mut self) -> [FanRpm; N];
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

/// Fold a published cooling config into the controller.
///
/// A gains change rebuilds it: the integral accumulated under the old gains
/// describes a fan this controller was not driving. Anything else only moves
/// the setpoint, which is idempotent, so it needs no comparison of its own.
///
/// The config watch publishes on any config change, so an unchanged cooling
/// section returns before logging and leaves an unrelated update silent here.
fn refresh_control(control: &mut TempControl, old: &CoolingConfig, new: &CoolingConfig) {
    if new == old {
        return;
    }
    let rebuild = new.pid != old.pid;
    if rebuild {
        control.reconfigure(new);
    } else {
        control.set_target(new.target_temp_c);
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
        config.target_temp_c,
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
/// consecutive sensor misses: `ubos-thermal` publishes nothing until its
/// `SENSOR_FAULT_TICKS` misses have elapsed, and only then starts
/// publishing `sensor_fault` readings every interval. The window must
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
/// which the failsafe overrides with full once the window elapses. A board that
/// configures `min_fan_pwm` to 0 therefore gets no airflow until the window
/// closes; the timeout, not the floor, is what bounds that exposure.
fn stale_timeout(thermal_sample_interval_ms: u32) -> Duration {
    let scaled = Duration::from_millis(
        u64::from(thermal_sample_interval_ms) * u64::from(STALE_INTERVAL_MULTIPLIER),
    );
    scaled.max(MIN_STALE_TIMEOUT)
}

/// Command the fan and publish the [`FanState`] plus per-fan metrics for the
/// given tachometer readings. The caller samples the tachometer on the control
/// tick and passes the most recent readings here, so a thermal-driven wake
/// republishes fresh PWM with the last sampled RPM rather than re-reading the
/// counter over a short, uneven window.
async fn emit_state<F, const N: usize>(
    fan: &mut F,
    pwm: FanPwm,
    rpms: &[FanRpm; N],
    tach_labels: &[Option<&'static str>; N],
) where
    F: FanOutput,
{
    fan.set_pwm(pwm);
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
/// forces full fan on `sensor_fault`, and resets the staleness clock; a
/// control tick that finds no reading for [`stale_timeout`] forces the fan
/// to 100%, so a thermal task wedged in a hung I2C read can never
/// leave the board uncooled. Either wake republishes telemetry, and the
/// fan starts at the configured floor, held at `min_fan_pwm` until the
/// first reading, so the display shows fan state from boot rather than
/// a blank line.
///
/// Cooling tuning is read from [`ubos_config::Handle`] and refreshed
/// between samples: a target-only change adjusts the setpoint in place, a
/// PID-gains change rebuilds the controller. This task is the sole writer
/// of the fan watch.
pub async fn run_cooling<F, T, const N: usize>(mut fan: F, mut tach: T, thermal: ThermalHandle) -> !
where
    F: FanOutput,
    T: TachReader<N>,
{
    let config_handle = ubos_config::handle();
    let mut config_subscription = config_handle.subscribe();
    let initial_config = config_handle.current();
    let mut config: CoolingConfig = initial_config.cooling;
    let mut stale = stale_timeout(initial_config.thermal.sample_interval_ms);

    let mut control = TempControl::from_config(&config);

    let tach_labels = T::LABELS;
    // Sample the tach once up front so boot telemetry is not a blank line; the
    // control tick refreshes it every CONTROL_INTERVAL thereafter.
    let mut last_rpms = tach.read_rpms();

    // Start the fan at the configured floor and publish immediately so the
    // display shows fan state from boot. min_fan_pwm flows straight through
    // from_percent with no special case, so a min_fan_pwm of 0 yields a
    // stopped fan by construction, held until the first reading.
    let mut last_pwm = FanPwm::from_percent(config.pid.min_fan_pwm);
    emit_state(&mut fan, last_pwm, &last_rpms, &tach_labels).await;
    log_cooling("cooling: started", &config);

    let mut subscription = thermal.subscribe();
    let mut ticker = Ticker::every(CONTROL_INTERVAL);
    let mut last_reading = Instant::now();
    // Set while the staleness failsafe holds the fan at full, so the forcing
    // warning fires once on entry and a matching recovery line fires once when
    // a thermal reading returns.
    let mut forced_full = false;
    loop {
        let event = select(subscription.changed(), ticker.next()).await;

        if let Some(updated) = config_subscription.try_changed() {
            let new_cooling = updated.cooling;
            refresh_control(&mut control, &config, &new_cooling);
            config = new_cooling;
            stale = stale_timeout(updated.thermal.sample_interval_ms);
        }

        match event {
            Either::First(reading) => {
                last_reading = Instant::now();
                if forced_full {
                    // Fires on the thermal task regaining liveness, regardless
                    // of this reading's sensor_fault: a fault reading still
                    // clears the stall because the task is alive, but the
                    // fault branch below keeps the fan at full, so recovery
                    // here does not imply the fan left 100%.
                    info!("cooling: thermal task recovered after stall");
                    forced_full = false;
                }
                // A run of sensor_fault readings skips control.update() each
                // time, so the smoother and ring buffer keep the last good
                // sample across that run. The derivative term on the first
                // non-fault reading is then computed against a pre-run value;
                // acceptable because the fault branch below parks the fan at
                // 100% on every reading of the run, so that stale derivative
                // never drives the fan. This covers the sensor_fault run only;
                // a thermal silence shorter than the staleness window triggers
                // no graduated response here: the control tick merely resamples
                // the tach and republishes telemetry until the window elapses.
                last_pwm = if reading.sensor_fault {
                    FanPwm::FULL
                } else {
                    FanPwm::from_percent(control.update(reading.pid_temp.celsius_f32()))
                };
                // Reuse the last sampled RPM; the tach is read on the control
                // tick, not here, so its window stays nominally fixed: delayed,
                // not skipped, if thermal readings monopolize the select.
                emit_state(&mut fan, last_pwm, &last_rpms, &tach_labels).await;
            }
            Either::Second(()) => {
                // Independent control tick. If the thermal task has gone
                // silent past the staleness window it is wedged and the PID
                // can no longer be trusted, so fail safe to full cooling.
                // Republish either way to keep telemetry fresh.
                let silence = last_reading.elapsed();
                if silence >= stale {
                    if !forced_full {
                        error!(
                            "cooling: no thermal reading for {} ms; forcing fan to 100%",
                            silence.as_millis()
                        );
                        forced_full = true;
                    }
                    last_pwm = FanPwm::FULL;
                }
                // Sample the tach once per tick: a nominally fixed CONTROL_INTERVAL
                // window for the edge count, independent of the thermal-reading cadence.
                last_rpms = tach.read_rpms();
                emit_state(&mut fan, last_pwm, &last_rpms, &tach_labels).await;
            }
        }
    }
}
