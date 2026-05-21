use super::*;

/// Encode a temperature in 0.125°C steps as the LM75A two-byte
/// register value.
fn encode(eighths_of_degree: i16) -> [u8; 2] {
    (eighths_of_degree << 5).to_be_bytes()
}

#[test]
fn zero() {
    assert_eq!(raw_to_millicelsius(encode(0)), 0);
}

#[test]
fn positive_integer_degree() {
    assert_eq!(raw_to_millicelsius(encode(25 * 8)), 25_000);
}

#[test]
fn positive_half_degree() {
    assert_eq!(raw_to_millicelsius(encode(25 * 8 + 4)), 25_500);
}

#[test]
fn positive_eighth_degree() {
    assert_eq!(raw_to_millicelsius(encode(25 * 8 + 1)), 25_125);
}

#[test]
fn negative_integer_degree() {
    assert_eq!(raw_to_millicelsius(encode(-25 * 8)), -25_000);
}

#[test]
fn negative_half_degree() {
    assert_eq!(raw_to_millicelsius(encode(-25 * 8 - 4)), -25_500);
}
