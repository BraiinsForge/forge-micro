//! Fan telemetry atomics shared between the board fan task (writer)
//! and any consumer that wants to display, log, or expose the values
//! (display task, HTTP/metrics endpoints). Lives in `ubos-core` so
//! every other ubos-* crate can read them without inducing a cycle
//! through `ubos-display` or the firmware-framework root.

use portable_atomic::{AtomicU32, AtomicU8, Ordering};

use crate::units::{FanPwm, FanRpm};

/// Last measured fan tach in RPM. Zero on boards without a fan, or
/// before the first tach sample completes on boards with one.
pub static FAN_RPM: AtomicU32 = AtomicU32::new(0);

/// Current fan PWM duty as an integer percent in the range 0 to 100
/// inclusive. `u8::MAX` is the sentinel for "this board has no fan";
/// consumers use it to decide whether to show fan info or fall back
/// to the BTC price. Boards without a fan leave the atomic at its
/// initial value, so no explicit sentinel write is needed.
pub static FAN_PWM_PERCENT: AtomicU8 = AtomicU8::new(u8::MAX);

/// Read the current PWM duty cycle. Returns [`FanPwm::ZERO`] when the
/// no-fan sentinel (`u8::MAX`) is stored, preserving the same semantic
/// as the `match u8::MAX => 0` blocks that previously appeared at each
/// call site.
pub fn load_pwm() -> FanPwm {
    match FAN_PWM_PERCENT.load(Ordering::Relaxed) {
        u8::MAX => FanPwm::ZERO,
        percent => FanPwm::from_percent(percent),
    }
}

/// Write a PWM duty cycle to the shared atomic.
pub fn store_pwm(pwm: FanPwm) {
    FAN_PWM_PERCENT.store(pwm.percent(), Ordering::Relaxed);
}

/// Read the current fan RPM.
pub fn load_rpm() -> FanRpm {
    FanRpm::from_rpm(FAN_RPM.load(Ordering::Relaxed))
}

/// Write the current fan RPM to the shared atomic.
pub fn store_rpm(rpm: FanRpm) {
    FAN_RPM.store(rpm.rpm(), Ordering::Relaxed);
}
