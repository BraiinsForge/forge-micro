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
