//! PID controller and thermal-target wrapper.
//!
//! Port of BOS `pid_control::PIDController` and
//! `bosminer-hal::fan::pid::TempControl` to f32 on no_std. Only the
//! `OnErrorWithSmoothing` derivative mode is kept: it is the only one
//! used by the BOS hashboard profile this crate carries forward.

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
/// Ported from BOS `pid_control::PIDController`. Only the
/// `OnErrorWithSmoothing` derivative mode is kept: it is the only mode
/// used by the BOS hashboard profile this crate carries forward.
/// Frozen integral runs at 15% gain to suppress quantization noise
/// once the system has settled, matching the BOS implementation.
pub(crate) struct PidController {
    p_gain: f32,
    i_gain: f32,
    d_gain: f32,
    /// `exp(ln(0.5) / smoother_half_decay_secs)`, the per-tick decay
    /// factor for the exponential smoother. Pre-computed at construction
    /// because `libm::expf` / `libm::logf` are not const fn. Matches BOS
    /// `pid_control` numerics in the nominal `delta_t == 1 s` case the
    /// monitor loop drives.
    decay_lambda: f32,
    out_min: f32,
    out_max: f32,
    err_sum: f32,
    prev_smoothed_error: Option<f32>,
    prev_derivative: f32,
}

impl PidController {
    #[must_use]
    pub fn new(p_gain: f32, i_gain: f32, d_gain: f32, smoother_half_decay_secs: f32) -> Self {
        let decay_lambda = libm::expf(libm::logf(0.5) / smoother_half_decay_secs);
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

    /// Run one PID step. `error` is `target - measurement`. `delta_t`
    /// is seconds since the previous `update`. `settled` enables noise
    /// suppression once the controller has settled: the derivative is
    /// frozen and the integral gain is damped to 0.15× of its nominal
    /// value.
    #[must_use]
    pub fn update(&mut self, error: f32, delta_t: f32, settled: bool) -> f32 {
        let p_term = self.p_gain * error;

        let i_gain = if settled {
            0.15 * self.i_gain
        } else {
            self.i_gain
        };
        self.err_sum = (self.err_sum + i_gain * error * delta_t).clamp(self.out_min, self.out_max);
        let i_term = self.err_sum;

        // BOS feeds `pid_control` a nominal `TICK_LENGTH` regardless of
        // wall-clock lateness, so `powf(decay_lambda, delta_t)` always
        // reduces to `decay_lambda`. Use it directly to keep libm out of
        // the hot path on softfloat targets.
        let derivative = if let Some(prev) = self.prev_smoothed_error {
            let new_smoothed = prev * self.decay_lambda + error * (1.0 - self.decay_lambda);
            let derivative = (new_smoothed - prev) / delta_t;
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
