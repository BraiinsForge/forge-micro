//! Tests for messages.

use super::*;

#[test]
fn subscribe_roundtrip() {
    let mut buf = [0u8; 256];
    let msg = Subscribe {
        agent_signature: Some("bmminer/2.0"),
        extra_nonce_1: None,
    };
    let n = encode_subscribe(&mut buf, 1, &msg)
        .expect("BUG: 256-byte buffer must fit subscribe message");
    let line =
        core::str::from_utf8(&buf[..n - 1]).expect("BUG: serde output is always valid UTF-8");
    assert!(line.contains(r#""id":1"#));
    assert!(line.contains(r#""mining.subscribe""#));
    assert!(line.contains("bmminer/2.0"));
}

#[test]
fn notify_deserialize_with_merkle_branch() {
    let params = br#"["job1","00000020abc","cb1hex","cb2hex",["aaa","bbb"],"20000000","1a0a2b3c","5f0d4e2f",true]"#;
    let (notify, _): (Notify<'_>, _) =
        serde_json_core::from_slice(params).expect("BUG: valid notify params must deserialize");
    assert_eq!(notify.job_id, "job1");
    assert_eq!(notify.merkle_branch.len(), 2);
    assert_eq!(notify.merkle_branch[0], "aaa");
    assert!(notify.clean_jobs);
}

#[test]
fn extranonce_subscribe_roundtrip() {
    let mut buf = [0u8; 128];
    let n = encode_extranonce_subscribe(&mut buf, 3)
        .expect("BUG: 128-byte buffer must fit extranonce.subscribe");
    let line =
        core::str::from_utf8(&buf[..n - 1]).expect("BUG: serde output is always valid UTF-8");
    assert!(line.contains(r#""id":3"#));
    assert!(line.contains(r#""mining.extranonce.subscribe""#));
    assert!(line.contains(r#""params":[]"#));
}

#[test]
fn ping_roundtrip() {
    let mut buf = [0u8; 128];
    let n = encode_ping(&mut buf, 9).expect("BUG: 128-byte buffer must fit ping");
    let line =
        core::str::from_utf8(&buf[..n - 1]).expect("BUG: serde output is always valid UTF-8");
    assert!(line.contains(r#""id":9"#));
    assert!(line.contains(r#""mining.ping""#));
}

#[test]
fn ping_response_shape() {
    let mut buf = [0u8; 64];
    let n = encode_ping_response(&mut buf, 42).expect("BUG: 64-byte buffer must fit ping response");
    assert_eq!(
        &buf[..n],
        br#"{"id":42,"result":null,"error":null}
"#
    );
}

#[test]
fn submit_roundtrip() {
    let mut buf = [0u8; 256];
    let msg = Submit {
        user_name: "worker1",
        job_id: "job42",
        extra_nonce_2: "00000001",
        time: "5f0d4e2f",
        nonce: "deadbeef",
        version: "20000000",
    };
    let n = encode_submit(&mut buf, 5, &msg).expect("BUG: 256-byte buffer must fit submit message");
    let line =
        core::str::from_utf8(&buf[..n - 1]).expect("BUG: serde output is always valid UTF-8");
    assert!(line.contains(r#""id":5"#));
    assert!(line.contains("worker1"));
    assert!(line.contains("deadbeef"));
}
