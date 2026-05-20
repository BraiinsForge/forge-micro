//! Fan telemetry atomics shared between the board fan task (writer)
//! and any consumer that wants to display, log, or expose the values
//! (display task, HTTP/metrics endpoints). Lives in `ubos-core` so
//! every other ubos-* crate can read them without inducing a cycle
//! through `ubos-display` or the firmware-framework root.

use portable_atomic::{AtomicU32, AtomicU8};

/// Last measured fan tach in RPM. Zero on boards without a fan, or
/// before the first tach sample completes on boards with one.
pub static FAN_RPM: AtomicU32 = AtomicU32::new(0);

/// Current fan PWM duty as an integer percent in the range 0 to 100
/// inclusive. `u8::MAX` is the sentinel for "this board has no fan";
/// consumers use it to decide whether to show fan info or fall back
/// to the BTC price.
pub static FAN_PWM_PERCENT: AtomicU8 = AtomicU8::new(u8::MAX);
