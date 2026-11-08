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

//! Unit tests for the PID port. Runs under host `cargo test`, where std
//! and floating point are available, so f64 helpers in `super::pid`
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
    assert!((min - 1.0).abs() < f64::EPSILON);
    assert!((max - 4.0).abs() < f64::EPSILON);
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
    assert!((min - 2.0).abs() < f64::EPSILON);
    assert!((max - 4.0).abs() < f64::EPSILON);
}

use super::pid::PidController;

#[test]
fn pid_proportional_only_clamped() {
    let mut pid = PidController::new(-5.0, 0.0, 0.0, 10.0);
    pid.set_limits(20.0, 100.0);
    let output = pid.update(50.0, false);
    assert!((output - 20.0).abs() < f64::EPSILON);
}

#[test]
fn pid_clamps_output_high() {
    let mut pid = PidController::new(-5.0, 0.0, 0.0, 10.0);
    pid.set_limits(20.0, 100.0);
    let output = pid.update(-50.0, false);
    assert!((output - 100.0).abs() < f64::EPSILON);
}

#[test]
fn pid_integral_accumulates() {
    let mut pid = PidController::new(0.0, -1.0, 0.0, 10.0);
    pid.set_limits(-1000.0, 1000.0);
    let first = pid.update(2.0, false);
    let second = pid.update(2.0, false);
    assert!(second < first, "integral should grow more negative");
}

#[test]
fn pid_freeze_dampens_integral() {
    let mut pid_free = PidController::new(0.0, -1.0, 0.0, 10.0);
    pid_free.set_limits(-1000.0, 1000.0);
    let mut pid_frozen = PidController::new(0.0, -1.0, 0.0, 10.0);
    pid_frozen.set_limits(-1000.0, 1000.0);

    let free = pid_free.update(2.0, false);
    let frozen = pid_frozen.update(2.0, true);
    assert!(frozen > free, "frozen integral term has smaller magnitude");
}

use super::pid::{TempControl, exponential_smoother, half_time_decay};
use ubos_core::units::{FanPwm, Temperature};

/// Match the BOS decay reference: lambda for a 30 s
/// half-decay is ~0.9772, and the smoother crosses 0.5 between
/// `decay_time - 1` and `decay_time + 1`.
#[test]
fn smoother_half_decay_matches_bos() {
    const EPS: f64 = 0.001;
    let lambda = half_time_decay(30.0);
    assert!((lambda - 0.9772).abs() < EPS);
    assert!(exponential_smoother(1.0, 0.0, 30.0, 29.0) > 0.5);
    assert!(exponential_smoother(1.0, 0.0, 30.0, 31.0) < 0.5);
}

/// Match the BOS iterate reference: stepping the smoother
/// in small increments matches a single step over the same total
/// duration. Proves the per-step path used by `PidController::update`
/// is consistent with the algebraic one-shot form.
#[test]
fn smoother_iterative_matches_one_shot() {
    const EPS: f64 = 0.005;
    let decay = 30.0_f64;
    let total = 32.0_f64;
    let step = 0.1_f64;
    let a = 1.0_f64;
    let b = 2.0_f64;
    let mut iterative = a;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let steps = libm::round(total / step) as usize;
    for _ in 0..steps {
        iterative = exponential_smoother(iterative, b, decay, step);
    }
    let one_shot = exponential_smoother(a, b, decay, total);
    assert!((iterative - one_shot).abs() < EPS);
}

/// A reading of `celsius` whole degrees.
fn degrees(celsius: i32) -> Temperature {
    Temperature::from_millicelsius(celsius * 1000)
}

#[test]
fn temp_control_clamps_min_pwm() {
    // Cold sample (temp below target) drives the PID negative, clamped
    // to the lower bound.
    let mut control = TempControl::new(-50.0, 0.0, 0.0, 10, 0, FanPwm::from_percent(20));
    control.set_target(degrees(67));
    let pwm = control.update(degrees(50));
    assert_eq!(pwm, FanPwm::from_percent(20));
}

#[test]
fn temp_control_clamps_max_pwm() {
    // Hot sample (temp above target) drives the PID positive, clamped
    // to the upper bound.
    let mut control = TempControl::new(-50.0, 0.0, 0.0, 10, 0, FanPwm::from_percent(20));
    control.set_target(degrees(67));
    let pwm = control.update(degrees(72));
    assert_eq!(pwm, FanPwm::from_percent(100));
}

/// A proportional-only controller whose duty reads back the temperature
/// the PID saw: with a gain of -1 against a target of 0 the duty is that
/// temperature in degrees, and a floor of 0 leaves it unclamped.
fn reads_back_input(input_smoother_half_decay_secs: u32) -> TempControl {
    let mut control = TempControl::new(
        -1.0,
        0.0,
        0.0,
        10,
        input_smoother_half_decay_secs,
        FanPwm::ZERO,
    );
    control.set_target(degrees(0));
    control
}

#[test]
fn input_smoother_of_zero_passes_a_jump_through() {
    let mut control = reads_back_input(0);
    assert_eq!(control.update(degrees(40)), FanPwm::from_percent(40));
    assert_eq!(control.update(degrees(80)), FanPwm::from_percent(80));
}

#[test]
fn input_smoother_moves_half_way_in_its_half_decay_time() {
    const HALF_DECAY_SECS: u32 = 10;
    let mut control = reads_back_input(HALF_DECAY_SECS);
    assert_eq!(control.update(degrees(40)), FanPwm::from_percent(40));
    // One tick keeps 2^(-1/10) of the old reading: 80 - 40 * 0.933 = 42.7.
    assert_eq!(control.update(degrees(80)), FanPwm::from_percent(43));
    // Ticks 2 to 9.
    for _ in 2..10 {
        let _ = control.update(degrees(80));
    }
    assert_eq!(control.update(degrees(80)), FanPwm::from_percent(60));
}

#[test]
fn forget_input_takes_the_next_sample_as_it_is() {
    let mut control = reads_back_input(10);
    assert_eq!(control.update(degrees(40)), FanPwm::from_percent(40));
    control.forget_input();
    assert_eq!(control.update(degrees(80)), FanPwm::from_percent(80));
}
