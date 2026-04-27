//! Unit tests for the PID port. Runs under host `cargo test`, where std
//! and floating point are available, so f32 helpers in `super::pid`
//! are exercised exactly as they will run on the embedded target.

use super::pid::RingBuffer;

#[test]
fn ring_buffer_min_max_empty() {
    let buffer: RingBuffer<3> = RingBuffer::new();
    assert_eq!(buffer.min_max(), None);
}

#[test]
fn ring_buffer_min_max_partial() {
    let mut buffer: RingBuffer<5> = RingBuffer::new();
    buffer.add(3.0);
    buffer.add(1.0);
    buffer.add(4.0);
    let (min, max) = buffer.min_max().expect("non-empty");
    assert!((min - 1.0).abs() < f32::EPSILON);
    assert!((max - 4.0).abs() < f32::EPSILON);
    assert!(!buffer.full());
}

#[test]
fn ring_buffer_overwrites_when_full() {
    let mut buffer: RingBuffer<3> = RingBuffer::new();
    buffer.add(1.0);
    buffer.add(2.0);
    buffer.add(3.0);
    assert!(buffer.full());
    buffer.add(4.0);
    let (min, max) = buffer.min_max().expect("non-empty");
    assert!((min - 2.0).abs() < f32::EPSILON);
    assert!((max - 4.0).abs() < f32::EPSILON);
}

use super::pid::PidController;

#[test]
fn pid_proportional_only_clamped() {
    let mut pid = PidController::new(-5.0, 0.0, 0.0, 10.0);
    pid.set_limits(20.0, 100.0);
    let output = pid.update(50.0, 1.0, false);
    assert!((output - 20.0).abs() < f32::EPSILON);
}

#[test]
fn pid_clamps_output_high() {
    let mut pid = PidController::new(-5.0, 0.0, 0.0, 10.0);
    pid.set_limits(20.0, 100.0);
    let output = pid.update(-50.0, 1.0, false);
    assert!((output - 100.0).abs() < f32::EPSILON);
}

#[test]
fn pid_integral_accumulates() {
    let mut pid = PidController::new(0.0, -1.0, 0.0, 10.0);
    pid.set_limits(-1000.0, 1000.0);
    let first = pid.update(2.0, 1.0, false);
    let second = pid.update(2.0, 1.0, false);
    assert!(second < first, "integral should grow more negative");
}

#[test]
fn pid_freeze_dampens_integral() {
    let mut pid_free = PidController::new(0.0, -1.0, 0.0, 10.0);
    pid_free.set_limits(-1000.0, 1000.0);
    let mut pid_frozen = PidController::new(0.0, -1.0, 0.0, 10.0);
    pid_frozen.set_limits(-1000.0, 1000.0);

    let free = pid_free.update(2.0, 1.0, false);
    let frozen = pid_frozen.update(2.0, 1.0, true);
    assert!(frozen > free, "frozen integral term has smaller magnitude");
}

use super::pid::{exponential_smoother, half_time_decay, TempControl};

/// Match BOS `pid_control::util::test::decay`: lambda for a 30 s
/// half-decay is ~0.9772, and the smoother crosses 0.5 between
/// `decay_time - 1` and `decay_time + 1`.
#[test]
fn smoother_half_decay_matches_bos() {
    const EPS: f32 = 0.001;
    let lambda = half_time_decay(30.0);
    assert!((lambda - 0.9772).abs() < EPS);
    assert!(exponential_smoother(1.0, 0.0, 30.0, 29.0) > 0.5);
    assert!(exponential_smoother(1.0, 0.0, 30.0, 31.0) < 0.5);
}

/// Match BOS `pid_control::util::test::iterate`: stepping the smoother
/// in small increments matches a single step over the same total
/// duration. Proves the per-step path used by `PidController::update`
/// is consistent with the algebraic one-shot form.
#[test]
fn smoother_iterative_matches_one_shot() {
    const EPS: f32 = 0.005;
    let decay = 30.0_f32;
    let total = 32.0_f32;
    let step = 0.1_f32;
    let a = 1.0_f32;
    let b = 2.0_f32;
    let mut iterative = a;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let steps = libm::roundf(total / step) as usize;
    for _ in 0..steps {
        iterative = exponential_smoother(iterative, b, decay, step);
    }
    let one_shot = exponential_smoother(a, b, decay, total);
    assert!((iterative - one_shot).abs() < EPS);
}

#[test]
fn temp_control_clamps_min_pwm() {
    // Cold sample (temp below target) drives the PID negative, clamped
    // to the lower bound.
    let mut control = TempControl::new(-50.0, 0.0, 0.0, 10.0, 20);
    control.set_target(67.0);
    let pwm = control.update(50.0, 1.0);
    assert_eq!(pwm, 20);
}

#[test]
fn temp_control_clamps_max_pwm() {
    // Hot sample (temp above target) drives the PID positive, clamped
    // to the upper bound.
    let mut control = TempControl::new(-50.0, 0.0, 0.0, 10.0, 20);
    control.set_target(67.0);
    let pwm = control.update(72.0, 1.0);
    assert_eq!(pwm, 100);
}
