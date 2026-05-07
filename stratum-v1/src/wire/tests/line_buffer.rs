//! Tests for `line_buffer`.

use super::*;

#[test]
fn single_chunk() {
    let mut lb = LineBuffer::<64>::new();
    let data = b"hello\n";
    lb.writable()[..data.len()].copy_from_slice(data);
    lb.advance(data.len());
    assert_eq!(
        lb.next_line()
            .expect("BUG: single complete line must parse"),
        Some(b"hello" as &[u8])
    );
    lb.consume();
    assert_eq!(
        lb.next_line()
            .expect("BUG: empty buffer must return Ok(None)"),
        None
    );
}

#[test]
fn two_advances() {
    let mut lb = LineBuffer::<64>::new();
    lb.writable()[..3].copy_from_slice(b"hel");
    lb.advance(3);
    assert_eq!(
        lb.next_line()
            .expect("BUG: partial line must return Ok(None)"),
        None
    );
    lb.writable()[..3].copy_from_slice(b"lo\n");
    lb.advance(3);
    assert_eq!(
        lb.next_line().expect("BUG: complete line must parse"),
        Some(b"hello" as &[u8])
    );
    lb.consume();
}

#[test]
fn two_lines_one_chunk() {
    let mut lb = LineBuffer::<64>::new();
    let data = b"line1\nline2\n";
    lb.writable()[..data.len()].copy_from_slice(data);
    lb.advance(data.len());
    assert_eq!(
        lb.next_line().expect("BUG: first line must parse"),
        Some(b"line1" as &[u8])
    );
    lb.consume();
    assert_eq!(
        lb.next_line().expect("BUG: second line must parse"),
        Some(b"line2" as &[u8])
    );
    lb.consume();
    assert_eq!(
        lb.next_line()
            .expect("BUG: empty buffer must return Ok(None)"),
        None
    );
}

#[test]
fn line_too_long() {
    let mut lb = LineBuffer::<8>::new();
    let data = b"toolong!"; // 8 bytes, no newline
    lb.writable()[..data.len()].copy_from_slice(data);
    lb.advance(data.len());
    assert!(matches!(lb.next_line(), Err(ParseError::LineTooLong)));
}
