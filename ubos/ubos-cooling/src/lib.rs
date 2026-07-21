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
#[path = "tests/pid.rs"]
mod tests_pid;

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::watch::Watch;
use ubos_config::CoolingConfig;
use ubos_core::units::{FanPwm, FanRpm};
use ubos_logging::info;
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
/// `read_rpms` once per control tick; the implementation returns the
/// speed of each fan measured since the previous call.
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
    /// Fastest tachometer reading at the last control tick. The display
    /// shows one fan line; on multi-fan boards this is the busiest fan.
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

fn build_control(config: &CoolingConfig) -> TempControl {
    let mut control = TempControl::new(
        config.pid.p_gain,
        config.pid.i_gain,
        config.pid.d_gain,
        config.pid.smoother_half_decay_secs,
        config.pid.min_fan_pwm,
    );
    control.set_target(config.target_temp_c);
    control
}

/// Run the cooling loop forever.
///
/// Acquires a long-lived `ThermalSubscription` once and then blocks on
/// `changed()` each iteration, so the PID fires at the same cadence
/// the thermal sensor publishes. Cooling tuning is read from
/// [`ubos_config::Handle`] and refreshed between samples: a target-only
/// change adjusts the setpoint in place, a PID-gains change rebuilds
/// the controller. If the reading carries `sensor_fault: true`, the
/// fan is forced to 100% and the PID state is not advanced.
///
/// Each tick reads the tachometer and publishes the commanded PWM and
/// measured RPM together as [`FanState`]; this task is the sole writer
/// of the fan watch.
pub async fn run_cooling<F, T, const N: usize>(mut fan: F, mut tach: T, thermal: ThermalHandle) -> !
where
    F: FanOutput,
    T: TachReader<N>,
{
    let publisher = STATE_WATCH.sender();
    let config_handle = ubos_config::handle();
    let mut config_subscription = config_handle.subscribe();
    let mut config: CoolingConfig = config_handle.current().cooling;

    let mut control = build_control(&config);
    info!("cooling: started (target={}°C)", config.target_temp_c);

    let mut subscription = thermal.subscribe();
    loop {
        let reading = subscription.changed().await;

        if let Some(updated) = config_subscription.try_changed() {
            let new_cooling = updated.cooling;
            if new_cooling.pid != config.pid {
                control = build_control(&new_cooling);
                info!(
                    "cooling: PID reset on gains update (target={}°C)",
                    new_cooling.target_temp_c
                );
            } else if new_cooling.target_temp_c.to_bits() != config.target_temp_c.to_bits() {
                control.set_target(new_cooling.target_temp_c);
                info!("cooling: target updated to {}°C", new_cooling.target_temp_c);
            }
            config = new_cooling;
        }

        // sensor_fault skips control.update(), so the smoother and ring
        // buffer keep the last good sample across the gap. The derivative
        // term on recovery is therefore computed against a pre-gap value;
        // acceptable because the fan is parked at 100% throughout the gap.
        let pwm = if reading.sensor_fault {
            FanPwm::FULL
        } else {
            let raw = control.update(reading.pid_temp.celsius_f32());
            FanPwm::from_percent(raw)
        };

        fan.set_pwm(pwm);
        let rpms = tach.read_rpms();
        let fastest = rpms
            .iter()
            .copied()
            .max_by_key(|reading| reading.rpm())
            .unwrap_or(FanRpm::from_rpm(0));
        publisher.send(FanState { pwm, rpm: fastest });

        push(Metric::with_no_labels(
            METRIC_FAN_PWM,
            Value::Uint(u64::from(pwm.percent())),
        ))
        .await;
        for (label, reading) in T::LABELS.iter().zip(rpms.iter()) {
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
}
