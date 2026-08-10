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
//! not part of that choice: a sensor fault and a stalled thermal task
//! force the fan to full in either mode.

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

use embassy_futures::select::{Either, select};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::watch::Watch;
use embassy_time::{Duration, Instant, Ticker};
use ubos_config::{CoolingConfig, FanMode};
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
/// forces full fan on `sensor_fault`, and resets the staleness clock; a
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
/// thermal cadence. Both failsafes still apply — a `sensor_fault` reading and
/// a lapsed staleness window force full in either mode — and the mining halt
/// at `thermal.dangerous_temp_c` is unaffected, so a low fixed duty costs
/// hashrate rather than the board.
///
/// Cooling tuning is read from [`ubos_config::Handle`] and refreshed
/// between samples: a target-only change adjusts the setpoint in place, a
/// PID-gains or mode change rebuilds the controller. This task is the sole
/// writer of the fan watch.
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

    // Start the fan at the configured floor, or at the fixed duty when the
    // operator has taken the fan off the PID, and publish immediately so the
    // display shows fan state from boot. Either percent flows straight through
    // from_percent with no special case, so a configured 0 yields a stopped
    // fan by construction, held until the first reading.
    let mut last_pwm = match config.mode {
        FanMode::Auto => FanPwm::from_percent(config.pid.min_fan_pwm),
        FanMode::Fixed => fixed_pwm(&config),
    };
    fan.set_pwm(last_pwm);

    // Sample the tach once up front so boot telemetry is not a blank line; the
    // control tick refreshes it every CONTROL_INTERVAL thereafter.
    let mut last_rpms = tach.read_rpms().await;
    publish_state(last_pwm, &last_rpms, &tach_labels).await;
    log_cooling("cooling: started", &config);

    let mut subscription = thermal.subscribe();
    let mut ticker = Ticker::every(CONTROL_INTERVAL);
    let mut last_reading = Instant::now();
    // Set while the staleness failsafe holds the fan at full, so the forcing
    // warning fires once on entry and a matching recovery line fires once when
    // a thermal reading returns.
    let mut forced_full = false;
    // Sensor-fault state of the last thermal reading. The reading arm parks
    // the fan at full on a fault; the control tick reads this so it does not
    // hand a fixed duty back to the fan between two fault readings.
    let mut sensor_fault = false;
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
                sensor_fault = reading.sensor_fault;
                last_pwm = if reading.sensor_fault {
                    FanPwm::FULL
                } else {
                    match config.mode {
                        // Fixed ignores the reading and never advances the
                        // PID; the mode transition back to auto rebuilds the
                        // controller, so no state is carried across.
                        FanMode::Fixed => fixed_pwm(&config),
                        FanMode::Auto => {
                            FanPwm::from_percent(control.update(reading.pid_temp.celsius_f32()))
                        }
                    }
                };
                fan.set_pwm(last_pwm);
                // Reuse the last sampled RPM; the tach is read on the control
                // tick, not here, so its window stays nominally fixed: delayed,
                // not skipped, if thermal readings monopolize the select.
                publish_state(last_pwm, &last_rpms, &tach_labels).await;
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
                } else if !sensor_fault {
                    // Fixed mode has no other cadence: thermal readings do not
                    // change the duty, so this tick is what carries a new
                    // fixed duty to the fan, within one CONTROL_INTERVAL of
                    // the config update. Auto raises the fan to the floor
                    // instead, which is what the transition out of fixed
                    // needs: the PID commands nothing until the next thermal
                    // reading, so a fixed duty below the floor, 0 included,
                    // would otherwise hold the fan for a whole sample
                    // interval after the operator left fixed mode. The PID
                    // clamps its own output to that same floor, so this only
                    // bites inside that window, and it raises the duty rather
                    // than lowering it. Skipped while the last reading was a
                    // fault, which parks the fan at full until one clears:
                    // ticks interleave with the fault readings, and taking
                    // this branch between two of them would cycle the fan.
                    last_pwm = match config.mode {
                        FanMode::Fixed => fixed_pwm(&config),
                        FanMode::Auto => last_pwm.max(FanPwm::from_percent(config.pid.min_fan_pwm)),
                    };
                }
                // Command the fan before the tach read: read_rpms can await a bus
                // transfer that never completes, and this tick's duty, the
                // staleness failsafe included, must not wait on it.
                fan.set_pwm(last_pwm);
                // Sample the tach once per tick: a nominally fixed CONTROL_INTERVAL
                // window for the edge count, independent of the thermal-reading cadence.
                last_rpms = tach.read_rpms().await;
                publish_state(last_pwm, &last_rpms, &tach_labels).await;
            }
        }
    }
}

/// Task shell around [`run_cooling`], the fan PID loop. Takes the
/// board's fan output and tachometer reader types.
#[macro_export]
macro_rules! cooling_task {
    ($fan:ty, $tach:ty) => {
        #[embassy_executor::task]
        async fn cooling_task(fan: $fan, tach: $tach, thermal: ubos_thermal::Handle) -> ! {
            ubos_cooling::run_cooling(fan, tach, thermal).await
        }
    };
}
