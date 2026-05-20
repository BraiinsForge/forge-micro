//! Board-thermal monitor: PID fan controller plus thermal-threshold guard.
//!
//! Reads the board temperature sensor through the shared
//! `TemperatureSensor` trait, drives a board-supplied `FanOutput` via
//! PID, and exposes `HALT_MINING` for a real ASIC backend to consult.

#![no_std]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::missing_panics_doc)]
#![allow(clippy::module_name_repetitions)]

pub mod pid;
pub mod threshold;

#[cfg(test)]
#[path = "tests/pid.rs"]
mod tests_pid;

#[cfg(test)]
#[path = "tests/threshold.rs"]
mod tests_threshold;

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Instant, Ticker};
use ubos_logging::{error, info, warn};
use ubos_metrics::metrics::{push, Metric, Value};
use ubos_temperature::{publish_board_temperature, TemperatureSensor};

/// Emergency-stop carrying the current halt state. `true` is raised
/// when the board temperature crosses `dangerous_temp_c`; `false` is
/// raised once it falls back below `cooldown_temp_c`. Mining backends
/// consume edges via `wait()` to cut the rail on halt and resume
/// bring-up on clear.
pub static HALT_MINING: Signal<CriticalSectionRawMutex, bool> = Signal::new();

use crate::pid::TempControl;
pub use crate::threshold::TempConfig;
use crate::threshold::{evaluate, Action};

/// Board-supplied fan PWM sink. `percent` is in `0..=100`. Boards
/// translate that to whatever counter top their PWM peripheral uses.
pub trait FanOutput {
    fn set_pwm_percent(&mut self, percent: u8);
}

/// PID controller tuning. Independent of the temperatures in
/// [`TempConfig`]: gains can be retuned without touching the
/// thermal setpoint or the halt logic and vice versa.
pub struct PidConfig {
    pub min_fan_pwm: u8,
    pub p_gain: f32,
    pub i_gain: f32,
    pub d_gain: f32,
    pub smoother_half_decay_secs: f32,
}

/// Tunable monitor parameters. Each board crate supplies its own
/// `MonitorConfig` calibrated for its thermal mass and fan curve.
pub struct MonitorConfig {
    pub temps: TempConfig,
    pub pid: PidConfig,
    pub sample_interval: Duration,
}

/// Thermal-monitor tuning matching the BOS hashboard profile for the
/// BMM101 (4×BM1362) board. Gains and thresholds carried forward from
/// the upstream `bosminer` configuration. Shared between firmware that
/// hosts the BMM hashboard (bmm-pico) and the chip-side mining backend
/// (ubos-miner + host-runner) so both halves stay tuned identically.
pub const BMM101_DEFAULT: MonitorConfig = MonitorConfig {
    temps: TempConfig {
        target_temp_c: 67.0,
        dangerous_temp_c: 90.0,
        cooldown_temp_c: 45.0,
    },
    pid: PidConfig {
        min_fan_pwm: 20,
        p_gain: -5.0,
        i_gain: -0.15,
        d_gain: -100.0,
        smoother_half_decay_secs: 10.0,
    },
    sample_interval: Duration::from_secs(1),
};

/// Fan PWM forced while halted. The PID is bypassed and 100% is
/// written every tick until the temperature falls back below
/// `cooldown_temp_c`, so a real ASIC chassis cools as fast as the
/// fan curve allows even if the integral term has wound up.
const HALTED_FAN_PWM: u8 = 100;

const METRIC_FAN_PWM: &str = "board_fan_pwm_percent";

/// Consecutive sensor-read failures tolerated before declaring the
/// bus dead. At a 1 Hz sample interval this is 5 seconds, long enough
/// to ride out a transient I2C glitch but short enough to react if
/// the bus has gone permanently silent (stuck SDA, dead chip, broken
/// cable). On fault the loop halts mining and parks the fan at 100%
/// until a successful reading clears it.
const SENSOR_FAULT_TICKS: u32 = 5;

/// Sample the sensor and track consecutive failures. On the rising
/// edge into fault (`misses == SENSOR_FAULT_TICKS`) raises
/// `HALT_MINING` and sets `halted`; while faulted, forces the fan to
/// `HALTED_FAN_PWM` and publishes the metric so a fault is not
/// silent on the dashboard. On the first successful reading after a
/// fault, releases halt so the threshold evaluator can decide afresh
/// from the new temperature.
async fn sample_with_fault<S, F>(
    sensor: &mut S,
    fan: &mut F,
    misses: &mut u32,
    halted: &mut bool,
) -> Option<i32>
where
    S: TemperatureSensor,
    F: FanOutput,
{
    let Some(value) = sensor.read_millicelsius().await else {
        *misses = misses.saturating_add(1);
        if *misses == SENSOR_FAULT_TICKS {
            error!(
                "monitor: sensor unreadable for {} ticks; halting + forcing fan to 100%",
                *misses
            );
            *halted = true;
            HALT_MINING.signal(true);
        }
        if *misses >= SENSOR_FAULT_TICKS {
            fan.set_pwm_percent(HALTED_FAN_PWM);
            push(Metric {
                name: METRIC_FAN_PWM,
                labels: heapless::Vec::new(),
                value: Value::Uint(u64::from(HALTED_FAN_PWM)),
                timestamp_ms: 0,
            })
            .await;
        }
        return None;
    };
    if *misses >= SENSOR_FAULT_TICKS {
        info!(
            "monitor: sensor recovered after {} misses; releasing halt for re-evaluation",
            *misses
        );
        *halted = false;
        HALT_MINING.signal(false);
    }
    *misses = 0;
    Some(value)
}

/// Run the monitor loop forever. Spawn this from a board firmware
/// crate, passing a board-specific `TemperatureSensor` and a
/// `FanOutput` over the board PWM peripheral.
pub async fn run_monitor<S, F>(mut sensor: S, mut fan: F, config: &'static MonitorConfig) -> !
where
    S: TemperatureSensor,
    F: FanOutput,
{
    let mut ticker = Ticker::every(config.sample_interval);
    let mut control = TempControl::new(
        config.pid.p_gain,
        config.pid.i_gain,
        config.pid.d_gain,
        config.pid.smoother_half_decay_secs,
        config.pid.min_fan_pwm,
    );
    control.set_target(config.temps.target_temp_c);

    let mut halted = false;
    let mut sensor_misses: u32 = 0;
    let mut prev_tick = Instant::now();
    // Matches BOS `monitor::main_loop`: warn when the tick fires more
    // than two periods late so a slow sensor read or a busy executor is
    // observable, since PID gains are tuned for the nominal one-tick
    // sample interval.
    let late_threshold = config.sample_interval * 2;

    info!("monitor: started (target={}°C)", config.temps.target_temp_c);

    loop {
        ticker.next().await;
        let now = Instant::now();
        let elapsed = now.duration_since(prev_tick);
        if elapsed > late_threshold {
            warn!("monitor: tick late by {} ms", elapsed.as_millis());
        }
        prev_tick = now;

        let Some(millicelsius) =
            sample_with_fault(&mut sensor, &mut fan, &mut sensor_misses, &mut halted).await
        else {
            continue;
        };
        #[allow(clippy::cast_precision_loss)]
        let temp_c = millicelsius as f32 / 1000.0;

        publish_board_temperature(millicelsius).await;

        let pid_pwm = control.update(temp_c);

        match evaluate(temp_c, halted, &config.temps) {
            Action::Ok => {}
            Action::EnterHalt => {
                halted = true;
                HALT_MINING.signal(true);
                error!(
                    "monitor: dangerous temperature {}°C, halting mining",
                    temp_c
                );
            }
            Action::Resume => {
                halted = false;
                HALT_MINING.signal(false);
                warn!("monitor: cooled to {}°C, resuming mining", temp_c);
            }
        }

        // Apply the override after the threshold action so EnterHalt
        // forces the fan immediately on the same tick that trips it.
        let pwm = if halted { HALTED_FAN_PWM } else { pid_pwm };
        fan.set_pwm_percent(pwm);

        push(Metric {
            name: METRIC_FAN_PWM,
            labels: heapless::Vec::new(),
            value: Value::Uint(u64::from(pwm)),
            timestamp_ms: 0,
        })
        .await;
    }
}
