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

use super::pid::TempControl;

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
