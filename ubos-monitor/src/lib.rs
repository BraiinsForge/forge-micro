//! Board-thermal monitor: PID fan controller plus thermal-threshold guard.
//!
//! Sibling of `ubos-backend`. Reads the board temperature sensor through
//! the shared `TemperatureSensor` trait, drives a board-supplied
//! `FanOutput` via PID, and exposes `HALT_MINING` for a real ASIC
//! backend to consult.

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
use eth_examples::metrics::{push, Metric, Value};
use ubos_logging::{error, info, warn};
use ubos_temperature::{Temperature, TemperatureSensor};

/// Emergency-stop carrying the current halt state. `true` is raised
/// when the board temperature crosses `dangerous_temp_c`; `false` is
/// raised once it falls back below `cooldown_temp_c`. Mining backends
/// consume edges via `wait()` to cut the rail on halt and resume
/// bring-up on clear.
pub static HALT_MINING: Signal<CriticalSectionRawMutex, bool> = Signal::new();

use crate::pid::TempControl;
use crate::threshold::{evaluate, Action, Limits};

/// Board-supplied fan PWM sink. `percent` is in `0..=100`. Boards
/// translate that to whatever counter top their PWM peripheral uses.
pub trait FanOutput {
    fn set_pwm_percent(&mut self, percent: u8);
}

/// Tunable monitor parameters. Each board crate supplies its own
/// `MonitorConfig` with gains and thresholds calibrated for its
/// thermal mass and fan curve.
pub struct MonitorConfig {
    pub target_temp_c: f32,
    pub hot_temp_c: f32,
    pub dangerous_temp_c: f32,
    pub cooldown_temp_c: f32,
    pub min_fan_pwm: u8,
    pub p_gain: f32,
    pub i_gain: f32,
    pub d_gain: f32,
    pub smoother_half_decay_secs: f32,
    pub sample_interval: Duration,
}

/// Fan PWM forced while halted. The PID is bypassed and 100% is
/// written every tick until the temperature falls back below
/// `cooldown_temp_c`, so a real ASIC chassis cools as fast as the
/// fan curve allows even if the integral term has wound up.
const HALTED_FAN_PWM: u8 = 100;

const METRIC_FAN_PWM: &str = "board_fan_pwm_percent";

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
        config.p_gain,
        config.i_gain,
        config.d_gain,
        config.smoother_half_decay_secs,
        config.min_fan_pwm,
    );
    control.set_target(config.target_temp_c);

    let limits = Limits {
        hot_temp_c: config.hot_temp_c,
        dangerous_temp_c: config.dangerous_temp_c,
        cooldown_temp_c: config.cooldown_temp_c,
    };

    #[allow(clippy::cast_precision_loss)]
    let delta_t = config.sample_interval.as_micros() as f32 / 1_000_000.0;

    let mut was_hot = false;
    let mut halted = false;
    let mut prev_tick = Instant::now();
    // Matches BOS `monitor::main_loop`: warn when the tick fires more
    // than two periods late so a slow sensor read or a busy executor is
    // observable instead of silently feeding a stale `delta_t` to the
    // PID.
    let late_threshold = config.sample_interval * 2;

    info!("monitor: started (target={}°C)", config.target_temp_c);

    loop {
        ticker.next().await;
        let now = Instant::now();
        let elapsed = now.duration_since(prev_tick);
        if elapsed > late_threshold {
            warn!("monitor: tick late by {} ms", elapsed.as_millis());
        }
        prev_tick = now;

        let Some(millicelsius) = sensor.read_millicelsius().await else {
            continue;
        };
        #[allow(clippy::cast_precision_loss)]
        let temp_c = millicelsius as f32 / 1000.0;

        push(Metric {
            name: Temperature::METRIC_NAME,
            labels: heapless::Vec::new(),
            value: Value::Int(i64::from(millicelsius)),
            timestamp_ms: 0,
        })
        .await;

        let pid_pwm = control.update(temp_c, delta_t);

        match evaluate(temp_c, halted, &limits) {
            Action::Ok => {
                if was_hot {
                    info!("monitor: cooled to {}°C", temp_c);
                    was_hot = false;
                }
            }
            Action::Hot => {
                if !was_hot {
                    warn!("monitor: hot {}°C pwm={}", temp_c, pid_pwm);
                    was_hot = true;
                }
            }
            Action::EnterHalt => {
                halted = true;
                HALT_MINING.signal(true);
                error!(
                    "monitor: dangerous temperature {}°C, halting mining",
                    temp_c
                );
                was_hot = false;
            }
            Action::Resume => {
                halted = false;
                HALT_MINING.signal(false);
                warn!("monitor: cooled to {}°C, resuming mining", temp_c);
                was_hot = false;
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
