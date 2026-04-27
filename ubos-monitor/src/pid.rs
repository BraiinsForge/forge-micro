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
