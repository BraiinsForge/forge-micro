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

//! PID controller and thermal-target wrapper.
//!
//! Port of the BOS PID controller and its temperature-target wrapper to
//! f32 on `no_std`. Only the `OnErrorWithSmoothing` derivative mode is
//! kept: it is the only one used by the BOS hashboard profile this
//! crate carries forward.

use ubos_config::CoolingConfig;

/// Fixed-capacity ring buffer used to detect a stable temperature window.
///
/// Stores the last `CAP` samples and reports their min/max so the PID can
/// freeze its derivative term once the system settles, avoiding
/// quantization-noise oscillations.
pub(crate) struct RingBuffer<const CAP: usize> {
    buffer: heapless::Deque<f32, CAP>,
}

impl<const CAP: usize> RingBuffer<CAP> {
    #[must_use]
    pub fn new() -> Self {
        Self {
            buffer: heapless::Deque::new(),
        }
    }

    pub fn add(&mut self, value: f32) {
        if self.buffer.is_full() {
            self.buffer.pop_front();
        }
        // `is_full()` was just resolved to false, so the push always fits.
        let _ = self.buffer.push_back(value);
    }

    #[must_use]
    pub fn full(&self) -> bool {
        self.buffer.is_full()
    }

    #[must_use]
    pub fn min_max(&self) -> Option<(f32, f32)> {
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

/// PID controller with optional exponentially-smoothed derivative.
///
/// Ported from the BOS PID controller. Only the `OnErrorWithSmoothing`
/// derivative mode is kept: it is the only mode used by the BOS
/// hashboard profile this crate carries forward.
/// Frozen integral runs at 15% gain to suppress quantization noise
/// once the system has settled, matching the BOS implementation.
pub(crate) struct PidController {
    p_gain: f32,
    i_gain: f32,
    d_gain: f32,
    /// `exp(ln(0.5) / smoother_half_decay_secs)`, the per-tick decay
    /// factor for the exponential smoother. Pre-computed at construction
    /// because `libm::expf` / `libm::logf` are not const fn. Matches the
    /// BOS numerics in the nominal `delta_t == 1 s` case the cooling
    /// loop drives.
    decay_lambda: f32,
    out_min: f32,
    out_max: f32,
    err_sum: f32,
    prev_smoothed_error: Option<f32>,
    prev_derivative: f32,
}

/// Per-second decay factor `lambda` such that `lambda^decay_secs == 0.5`.
/// Pre-computed at PID construction so the per-step path needs only
/// `powf(lambda, dt)` instead of `expf` / `logf`.
#[must_use]
pub fn half_time_decay(decay_secs: f32) -> f32 {
    libm::expf(libm::logf(0.5) / decay_secs)
}

/// One step of an exponentially-smoothed lowpass: blend `value` into
/// `prev` over `decay_secs` half-life, advancing `dt` seconds. Used by
/// tests against the BOS reference implementation; the production hot
/// path inlines the same calculation in `PidController::update` with a
/// cached lambda.
#[cfg(test)]
#[must_use]
pub fn exponential_smoother(prev: f32, value: f32, decay_secs: f32, dt: f32) -> f32 {
    let alpha = half_time_decay(decay_secs);
    let pow_alpha = libm::powf(alpha, dt);
    prev * pow_alpha + value * (1.0 - pow_alpha)
}

impl PidController {
    #[must_use]
    pub fn new(p_gain: f32, i_gain: f32, d_gain: f32, smoother_half_decay_secs: f32) -> Self {
        let decay_lambda = half_time_decay(smoother_half_decay_secs);
        Self {
            p_gain,
            i_gain,
            d_gain,
            decay_lambda,
            out_min: f32::NEG_INFINITY,
            out_max: f32::INFINITY,
            err_sum: 0.0,
            prev_smoothed_error: None,
            prev_derivative: 0.0,
        }
    }

    pub fn set_limits(&mut self, min: f32, max: f32) {
        self.out_min = min;
        self.out_max = max;
        self.err_sum = self.err_sum.clamp(min, max);
    }

    /// Run one PID step. `error` is `target - measurement`. `settled`
    /// enables noise suppression once the controller has settled: the
    /// derivative is frozen and the integral gain is damped to 0.15× of
    /// its nominal value. Per-tick scaling is implicit; BOS likewise
    /// feeds its PID a constant `TICK_LENGTH`, so the gains absorb the
    /// nominal one-second-per-tick assumption.
    #[must_use]
    pub fn update(&mut self, error: f32, settled: bool) -> f32 {
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

/// Temperature-driven fan-PWM controller.
///
/// Wraps `PidController` with a target setpoint, a stability detector
/// (the ring buffer), and integer-percent output clamping. Mirrors the
/// BOS temperature-target controller in its single-input case.
pub struct TempControl {
    pid: PidController,
    history: RingBuffer<{ Self::HISTORY_SIZE }>,
    target_temp_c: f32,
    min_fan_pwm: u8,
}

impl TempControl {
    /// Window of samples checked for stability. 60 samples at the
    /// 1 Hz cooling cadence is one minute of history, the same length
    /// the BOS controller uses.
    const HISTORY_SIZE: usize = 60;
    /// Maximum spread, in degrees Celsius, between the min and max of
    /// the history window for the system to count as stable.
    const STABLE_MAX_TEMP_DIFF_C: f32 = 1.6;
    /// Only freeze the derivative once the absolute error is within
    /// this many degrees of the target.
    const CLOSE_TO_TARGET_C: f32 = 3.1;

    #[must_use]
    pub fn new(
        p_gain: f32,
        i_gain: f32,
        d_gain: f32,
        smoother_half_decay_secs: f32,
        min_fan_pwm: u8,
    ) -> Self {
        // PWM is reported as a percent, so a min above 100 would invert
        // the clamp range and panic in `update`. The debug_assert traps a
        // bad value in tests; in release the warn surfaces the corrupt
        // persisted config and the clamp keeps the firmware panic-free.
        debug_assert!(min_fan_pwm <= 100, "min_fan_pwm must be <= 100");
        if min_fan_pwm > 100 {
            ubos_logging::warn!("min_fan_pwm {} exceeds 100, clamping to 100", min_fan_pwm);
        }
        let min_fan_pwm = min_fan_pwm.min(100);
        let mut pid = PidController::new(p_gain, i_gain, d_gain, smoother_half_decay_secs);
        pid.set_limits(f32::from(min_fan_pwm), 100.0);
        Self {
            pid,
            history: RingBuffer::new(),
            target_temp_c: 0.0,
            min_fan_pwm,
        }
    }

    /// Build a controller from the cooling section of the config: the gains
    /// and the output floor for the PID, and the thermal setpoint.
    #[must_use]
    pub fn from_config(config: &CoolingConfig) -> Self {
        let mut control = Self::new(
            config.pid.p_gain,
            config.pid.i_gain,
            config.pid.d_gain,
            config.pid.smoother_half_decay_secs,
            config.pid.min_fan_pwm,
        );
        control.set_target(config.target_temp_c);
        control
    }

    /// Reconfigure for `config`, discarding the accumulated control state.
    ///
    /// The integral, the derivative smoother, and the stability window all
    /// describe the fan the previous configuration was driving, so new gains
    /// — or a return from a fixed duty, which the controller did not drive at
    /// all — must start clean rather than carry them forward. Replacing the
    /// whole value is what makes that total: no field survives by omission.
    pub fn reconfigure(&mut self, config: &CoolingConfig) {
        *self = Self::from_config(config);
    }

    pub fn set_target(&mut self, target_temp_c: f32) {
        self.target_temp_c = target_temp_c;
    }

    /// Compute the next fan PWM percent (`min_fan_pwm..=100`) from the
    /// latest temperature sample. Per-tick scaling is implicit: callers
    /// must invoke `update` once per sample tick for the PID gains to
    /// behave as tuned.
    #[must_use]
    pub fn update(&mut self, temp_c: f32) -> u8 {
        let error = self.target_temp_c - temp_c;
        self.history.add(error);

        let settled = self.history.min_max().is_some_and(|(min, max)| {
            let stable = max - min < Self::STABLE_MAX_TEMP_DIFF_C;
            let close = libm::fabsf(error) < Self::CLOSE_TO_TARGET_C;
            self.history.full() && stable && close
        });

        let raw = self.pid.update(error, settled);
        // PID output is already clamped to [min_fan_pwm, 100].
        // u8 cast is safe because of the clamp.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let pwm = libm::roundf(raw) as u8;
        pwm.clamp(self.min_fan_pwm, 100)
    }
}
