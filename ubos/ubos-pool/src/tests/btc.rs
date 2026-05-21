use super::*;

#[test]
fn typical_binance_response() {
    let response = br#"HTTP/1.0 200 OK\r\n\r\n{"symbol":"BTCUSDT","price":"104321.57000000"}"#;
    assert_eq!(parse_price_cents(response), Some(10_432_157));
}

#[test]
fn no_decimal() {
    let response = br#"{"price":"50000"}"#;
    assert_eq!(parse_price_cents(response), Some(5_000_000));
}

#[test]
fn single_decimal_digit() {
    let response = br#"{"price":"100.5"}"#;
    assert_eq!(parse_price_cents(response), Some(10_050));
}

#[test]
fn missing_price_field() {
    let response = b"HTTP/1.0 200 OK\r\n\r\n{}";
    assert_eq!(parse_price_cents(response), None);
}

#[test]
fn empty_input() {
    assert_eq!(parse_price_cents(b""), None);
}
