//! Fan PID controller task: subscribes to the thermal subsystem and
//! drives the board fan via the `FanOutput` trait. Generic over the
//! thermal source and fan output to stay portable across boards.

#![no_std]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::missing_panics_doc)]
#![allow(clippy::module_name_repetitions)]

mod pid;

#[cfg(test)]
#[path = "tests/pid.rs"]
mod tests_pid;

use ubos_config::CoolingConfig;
use ubos_core::fan_state;
use ubos_core::units::FanPwm;
use ubos_logging::info;
use ubos_metrics::metrics::{push, Metric, Value};
use ubos_thermal::Handle as ThermalHandle;

use crate::pid::TempControl;

/// Board-supplied fan PWM sink. Boards translate the [`FanPwm`] duty
/// cycle to whatever counter top their PWM peripheral uses.
pub trait FanOutput {
    fn set_pwm(&mut self, pwm: FanPwm);
}

/// Tachometer reader. Declared here for Step 4; no implementations or
/// callers exist yet.
pub trait TachReader {
    fn read_rpm(&mut self) -> ubos_core::units::FanRpm;
}

const METRIC_FAN_PWM: &str = "board_fan_pwm_percent";

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
pub async fn run_cooling<F: FanOutput>(mut fan: F, thermal: ThermalHandle) -> ! {
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
            let raw = control.update(reading.chip_max.celsius_f32());
            FanPwm::from_percent(raw)
        };

        fan.set_pwm(pwm);
        fan_state::store_pwm(pwm);

        push(Metric::with_no_labels(
            METRIC_FAN_PWM,
            Value::Uint(u64::from(pwm.percent())),
        ))
        .await;
    }
}
