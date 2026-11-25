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

//! The PID controller and its temperature-target wrapper, in `f64` on
//! `no_std`. Only the smoothed-derivative mode is kept, the one the
//! hashboard profile this crate carries forward uses.

use ubos_config::CoolingConfig;
use ubos_core::units::{FanPwm, Temperature};

/// The last `CAP` samples and their spread, so the PID can freeze its
/// derivative once the system settles and stop oscillating on
/// quantization noise.
pub(crate) struct RingBuffer<const CAP: usize> {
    buffer: heapless::Deque<f64, CAP>,
}

impl<const CAP: usize> RingBuffer<CAP> {
    #[must_use]
    pub fn new() -> Self {
        Self {
            buffer: heapless::Deque::new(),
        }
    }

    pub fn add(&mut self, value: f64) {
        if self.buffer.is_full() {
            self.buffer.pop_front();
        }
        let _ = self.buffer.push_back(value);
    }

    #[must_use]
    pub fn full(&self) -> bool {
        self.buffer.is_full()
    }

    #[must_use]
    pub fn min_max(&self) -> Option<(f64, f64)> {
        let mut iter = self.buffer.iter().copied();
        let first = iter.next()?;
        Some(iter.fold((first, first), |(min, max), value| {
            (
                if value < min { value } else { min },
                if value > max { value } else { max },
            )
        }))
    }
}

impl<const CAP: usize> Default for RingBuffer<CAP> {
    fn default() -> Self {
        Self::new()
    }
}

/// PID controller with an exponentially-smoothed derivative. Once
/// settled the integral runs at 15% gain, which suppresses quantization
/// noise.
pub(crate) struct PidController {
    p_gain: f64,
    i_gain: f64,
    d_gain: f64,
    /// The smoother's per-tick decay factor, computed at construction
    /// because `exp` and `log` are not `const fn`.
    decay_lambda: f64,
    out_min: f64,
    out_max: f64,
    err_sum: f64,
    prev_smoothed_error: Option<f64>,
    prev_derivative: f64,
}

/// The decay factor `lambda` with `lambda^decay_secs == 0.5`, so the
/// per-step path needs only a `pow`.
#[must_use]
pub fn half_time_decay(decay_secs: f64) -> f64 {
    libm::exp(libm::log(0.5) / decay_secs)
}

/// One step of the smoothed lowpass. The production path inlines the
/// same calculation with a cached lambda; this is for the tests.
#[cfg(test)]
#[must_use]
pub fn exponential_smoother(prev: f64, value: f64, decay_secs: f64, dt: f64) -> f64 {
    let alpha = half_time_decay(decay_secs);
    let pow_alpha = libm::pow(alpha, dt);
    prev * pow_alpha + value * (1.0 - pow_alpha)
}

impl PidController {
    #[must_use]
    pub fn new(p_gain: f64, i_gain: f64, d_gain: f64, smoother_half_decay_secs: f64) -> Self {
        let decay_lambda = half_time_decay(smoother_half_decay_secs);
        Self {
            p_gain,
            i_gain,
            d_gain,
            decay_lambda,
            out_min: f64::NEG_INFINITY,
            out_max: f64::INFINITY,
            err_sum: 0.0,
            prev_smoothed_error: None,
            prev_derivative: 0.0,
        }
    }

    pub fn set_limits(&mut self, min: f64, max: f64) {
        self.out_min = min;
        self.out_max = max;
        self.err_sum = self.err_sum.clamp(min, max);
    }

    /// One PID step over `error`, which is `target - measurement`.
    /// `settled` freezes the derivative and damps the integral gain.
    /// Per-tick scaling is implicit: the gains absorb the nominal
    /// one-second tick.
    #[must_use]
    pub fn update(&mut self, error: f64, settled: bool) -> f64 {
        let p_term = self.p_gain * error;

        let i_gain = if settled {
            0.15 * self.i_gain
        } else {
            self.i_gain
        };
        self.err_sum = (self.err_sum + i_gain * error).clamp(self.out_min, self.out_max);
        let i_term = self.err_sum;

        let derivative = if let Some(prev) = self.prev_smoothed_error {
            let new_smoothed = prev * self.decay_lambda + error * (1.0 - self.decay_lambda);
            let derivative = new_smoothed - prev;
            self.prev_smoothed_error = Some(new_smoothed);
            derivative
        } else {
            self.prev_smoothed_error = Some(error);
            0.0
        };
        let derivative = if settled {
            self.prev_derivative
        } else {
            derivative
        };
        let d_term = self.d_gain * derivative;

        self.prev_derivative = derivative;

        (p_term + d_term + i_term).clamp(self.out_min, self.out_max)
    }
}

/// A [`PidController`] with a setpoint, a smoother on the temperature it
/// reads, the stability window and integer-percent output clamping.
pub struct TempControl {
    pid: PidController,
    history: RingBuffer<{ Self::HISTORY_SIZE }>,
    target: Temperature,
    min_fan_pwm: FanPwm,
    /// Per-tick decay factor of the input smoother, from
    /// [`half_time_decay`]. Zero passes the reading through.
    input_decay_lambda: f64,
    /// The smoothed temperature, absent until the first sample.
    smoothed_temp_c: Option<f64>,
}

impl TempControl {
    /// One minute of history at the cooling cadence.
    const HISTORY_SIZE: usize = 60;
    /// Spread across the window under which the system counts as stable.
    const STABLE_MAX_TEMP_DIFF_C: f64 = 1.6;
    /// The derivative freezes only this close to the target.
    const CLOSE_TO_TARGET_C: f64 = 3.1;

    #[must_use]
    pub fn new(
        p_gain: f64,
        i_gain: f64,
        d_gain: f64,
        smoother_half_decay_secs: u32,
        input_smoother_half_decay_secs: u32,
        min_fan_pwm: FanPwm,
    ) -> Self {
        let mut pid =
            PidController::new(p_gain, i_gain, d_gain, f64::from(smoother_half_decay_secs));
        pid.set_limits(
            f64::from(min_fan_pwm.percent()),
            f64::from(FanPwm::FULL.percent()),
        );
        Self {
            pid,
            history: RingBuffer::new(),
            target: Temperature::from_millicelsius(0),
            min_fan_pwm,
            input_decay_lambda: half_time_decay(f64::from(input_smoother_half_decay_secs)),
            smoothed_temp_c: None,
        }
    }

    #[must_use]
    pub fn from_config(config: &CoolingConfig) -> Self {
        // `FanPwm` clamps to 100, so a floor above it is caught here, where
        // the persisted percent arrives. The debug_assert traps a bad value
        // in tests; in release the warn surfaces the corrupt config.
        let min_fan_pwm = config.pid.min_fan_pwm;
        debug_assert!(min_fan_pwm <= 100, "min_fan_pwm must be <= 100");
        if min_fan_pwm > 100 {
            ubos_logging::warn!("min_fan_pwm {} exceeds 100, clamping to 100", min_fan_pwm);
        }
        let mut control = Self::new(
            config.pid.p_gain,
            config.pid.i_gain,
            config.pid.d_gain,
            config.pid.smoother_half_decay_secs,
            config.pid.input_smoother_half_decay_secs,
            FanPwm::from_percent(min_fan_pwm),
        );
        control.set_target(config.target.temp);
        control
    }

    /// Reconfigure, discarding the control state: the integral, the
    /// smoother and the window all describe the fan the previous
    /// configuration was driving. The whole value is replaced, so no
    /// field survives by omission.
    pub fn reconfigure(&mut self, config: &CoolingConfig) {
        *self = Self::from_config(config);
    }

    pub fn set_target(&mut self, target: Temperature) {
        self.target = target;
    }

    /// Drop the smoothed temperature, so the next sample is taken as it
    /// is. A caller that skipped samples uses it, because the board may
    /// have moved far from the smoothed value in the gap.
    pub fn forget_input(&mut self) {
        self.smoothed_temp_c = None;
    }

    /// The next fan percent from the latest sample. Called once per
    /// sample tick, or the gains do not behave as tuned.
    #[must_use]
    pub fn update(&mut self, temp: Temperature) -> FanPwm {
        let error = self.target.celsius_f64() - self.smooth(temp);
        self.history.add(error);

        let settled = self.history.min_max().is_some_and(|(min, max)| {
            let stable = max - min < Self::STABLE_MAX_TEMP_DIFF_C;
            let close = libm::fabs(error) < Self::CLOSE_TO_TARGET_C;
            self.history.full() && stable && close
        });

        let raw = self.pid.update(error, settled);
        // Already clamped to the floor and 100, so the cast is safe.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let pwm = FanPwm::from_percent(libm::round(raw) as u8);
        pwm.max(self.min_fan_pwm)
    }

    /// Blend `temp` into the smoothed temperature and return it in degrees
    /// Celsius. The first sample is taken as it is. The smoothed value
    /// is fractional and stays a float between samples, since rounding it
    /// to millicelsius each tick would change the filter.
    fn smooth(&mut self, temp: Temperature) -> f64 {
        let temp_c = temp.celsius_f64();
        let lambda = self.input_decay_lambda;
        let smoothed = self
            .smoothed_temp_c
            .map_or(temp_c, |prev| prev * lambda + temp_c * (1.0 - lambda));
        self.smoothed_temp_c = Some(smoothed);
        smoothed
    }
}
