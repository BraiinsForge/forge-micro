//! Tests for parse.

use super::*;

use crate::wire::messages::Notify;
use crate::wire::parse::{
    parse_boolean_result, parse_incoming, parse_subscribe_result, parse_version_rolling_mask,
};
use crate::wire::rpc::{Notification, Rpc};

// Shared 12-branch merkle path
const MERKLE_BRANCH: &str = concat!(
    "[\"ae23055e00f0f697cc3640124812d96d4fe8bdfa03484c1c638ce5a1c0e9aa81\",",
    "\"980fb87cb61021dd7afd314fcb0dabd096f3d56a7377f6f320684652e7410a21\",",
    "\"a52e9868343c55ce405be8971ff340f562ae9ab6353f07140d01666180e19b52\",",
    "\"7435bdfa004e603953b2ed39f118803934d9cf17b06d979ceb682f2251bafac2\",",
    "\"2a91f061a22d27cb8f44eea79938fb241ebeb359891aa907f05ffde7ed44e52e\",",
    "\"302401f80eb5e958155135e25200bb8ea181ad2d05e804a531c7314d86403cdc\",",
    "\"318ecb6161eb9b4cfd802bd730e2d36c167ddf102e70aa7b4158e2870dd47392\",",
    "\"1114332a9858e0cf84b2425bb1e59eaabf91dd102d114aa443d57fc1b3beb0c9\",",
    "\"f43f38095c810613ed795a44d9fab02ff25269706f454885db9be05cdf9c06e1\",",
    "\"3e2fc26b27fddc39668b59099cd9635761bb72ed92404204e12bdff08b16fb75\",",
    "\"463c19427286342120039a83218fa87ce45448e246895abac11fff0036076758\",",
    "\"03d287f655813e540ddb9c4e7aeb922478662b0f5d8e9d0cbd564b20146bab76\"]"
);

const COINBASE_1: &str = "01000000010000000000000000000000000000000000000000000000000000000000000000ffffffff4b03a5020cfabe6d6d379ae882651f6469f2ed6b8b40a4f9a4b41fd838a3ad6de8cba775f4e8f1d3080100000000000000";
const COINBASE_2: &str = "41903d4c1b2f736c7573682f0000000003ca890d27000000001976a9147c154ed1dc59609e3d26abb2df2ea3d587cd8c4188ac00000000000000002c6a4c2952534b424c4f434b3a4cb4cb2ddfc37c41baf5ef6b6b4899e3253a8f1dfc7e5dd68a5b5b27005014ef0000000000000000266a24aa21a9ed5caa249f1af9fbf71c986fea8e076ca34ae3514fb2f86400561b28c7b15949bf00000000";
const PREV_HASH: &str = "ef4b9a48c7986466de4adc002f7337a6e121bc43000376ea0000000000000000";

fn build_notify_line(job_id: &str, clean_jobs: &str) -> heapless::String<4096> {
    let mut line = heapless::String::<4096>::new();
    line.push_str("{\"id\":null,\"method\":\"mining.notify\",\"params\":[\"")
        .expect("BUG: fixture fits within 4096 bytes");
    line.push_str(job_id).expect("BUG: fixture fits");
    line.push_str("\",\"").expect("BUG: fixture fits");
    line.push_str(PREV_HASH).expect("BUG: fixture fits");
    line.push_str("\",\"").expect("BUG: fixture fits");
    line.push_str(COINBASE_1).expect("BUG: fixture fits");
    line.push_str("\",\"").expect("BUG: fixture fits");
    line.push_str(COINBASE_2).expect("BUG: fixture fits");
    line.push_str("\",").expect("BUG: fixture fits");
    line.push_str(MERKLE_BRANCH).expect("BUG: fixture fits");
    line.push_str(",\"20000004\",\"1705c739\",\"64495522\",")
        .expect("BUG: fixture fits");
    line.push_str(clean_jobs).expect("BUG: fixture fits");
    line.push_str("]}").expect("BUG: fixture fits");
    line
}

fn expect_notify<'a>(rpc: &'a Rpc<'a>) -> &'a Notify<'a> {
    match rpc {
        Rpc::Notification(Notification::Notify(notify)) => notify,
        other => panic!("expected Notify, got {:?}", other),
    }
}

#[test]
fn parse_mining_notify_standard() {
    let line = build_notify_line("1b4c3d9041", "false");
    let rpc = parse_incoming(line.as_bytes()).expect("BUG: valid notify must parse");
    let notify = expect_notify(&rpc);
    assert!(!notify.clean_jobs);
}

#[test]
fn parse_mining_notify_abandon_work_variants() {
    let false_line = build_notify_line("1b4c3d9041", "false");
    let rpc = parse_incoming(false_line.as_bytes()).expect("BUG: valid notify must parse");
    assert!(!expect_notify(&rpc).clean_jobs);

    let true_line = build_notify_line("1b4c3d9041", "true");
    let rpc = parse_incoming(true_line.as_bytes()).expect("BUG: valid notify must parse");
    assert!(expect_notify(&rpc).clean_jobs);
}

// Some pools might send a 10th trailing element after clean_jobs
// in notify. We reject it.
#[test]
fn parse_mining_notify_trailing_element_is_rejected() {
    let mut line = heapless::String::<4096>::new();
    let base = build_notify_line("1b4c3d9041", "true");
    // Drop trailing "]}" and append extra element.
    let prefix = &base[..base.len() - 2];
    line.push_str(prefix).expect("BUG: fixture fits");
    line.push_str(",\"extra\"]}").expect("BUG: fixture fits");
    assert!(parse_incoming(line.as_bytes()).is_err());
}

#[test]
fn parse_mining_notify_field_extraction() {
    let line = build_notify_line("1d2e0c4d3d", "false");
    let rpc = parse_incoming(line.as_bytes()).expect("BUG: valid notify must parse");
    let notify = expect_notify(&rpc);
    assert_eq!(notify.job_id, "1d2e0c4d3d");
    assert_eq!(notify.prev_hash, PREV_HASH);
    assert_eq!(notify.coin_base_1, COINBASE_1);
    assert_eq!(notify.coin_base_2, COINBASE_2);
    assert_eq!(notify.version, "20000004");
    assert_eq!(notify.bits, "1705c739");
    assert_eq!(notify.time, "64495522");
    assert_eq!(notify.merkle_branch.len(), 12);
    assert_eq!(
        notify.merkle_branch[0],
        "ae23055e00f0f697cc3640124812d96d4fe8bdfa03484c1c638ce5a1c0e9aa81"
    );
    assert!(!notify.clean_jobs);
}

#[test]
fn parse_set_difficulty_integer() {
    let line = br#"{"id":null,"method":"mining.set_difficulty","params":[1638]}"#;
    let rpc = parse_incoming(line).expect("BUG: valid set_difficulty must parse");
    match rpc {
        Rpc::Notification(Notification::SetDifficulty(msg)) => {
            assert!((msg.0 - 1638.0).abs() < f64::EPSILON);
        }
        other => panic!("expected SetDifficulty, got {:?}", other),
    }
}

#[test]
fn parse_set_difficulty_fractional() {
    let line = br#"{"id":null,"method":"mining.set_difficulty","params":[100.5]}"#;
    let rpc = parse_incoming(line).expect("BUG: valid set_difficulty must parse");
    match rpc {
        Rpc::Notification(Notification::SetDifficulty(msg)) => {
            assert!((msg.0 - 100.5).abs() < f64::EPSILON);
        }
        other => panic!("expected SetDifficulty, got {:?}", other),
    }
}

#[test]
fn parse_subscribe_result_real_pool() {
    let line = br#"{"result":[[["mining.notify","695482c0"]],"4de05269",8],"id":2,"error":null}"#;
    let rpc = parse_incoming(line).expect("BUG: valid subscribe response must parse");
    let response = match rpc {
        Rpc::Response(response) => response,
        Rpc::Notification(_) => panic!("expected Response"),
    };
    assert_eq!(response.id, 2);
    let body = response
        .stratum_result
        .expect("BUG: result present in fixture");
    let subscribe_result = parse_subscribe_result(body).expect("BUG: subscribe body must parse");
    assert_eq!(subscribe_result.1, "4de05269");
    assert_eq!(subscribe_result.2, 8);
}

#[test]
fn parse_set_version_mask() {
    let line = br#"{"id":1,"method":"mining.set_version_mask","params":["1fffe000"]}"#;
    let rpc = parse_incoming(line).expect("BUG: valid set_version_mask must parse");
    match rpc {
        Rpc::Notification(Notification::SetVersionMask(msg)) => {
            assert_eq!(msg.mask, 0x1fff_e000);
        }
        other => panic!("expected SetVersionMask, got {:?}", other),
    }
}

#[test]
fn parse_result_bool_true_ids() {
    for (raw, expected_id) in [
        (&br#"{"id":4,"error":null,"result":true}"#[..], 4u32),
        (&br#"{"id":5,"error":null,"result":true}"#[..], 5),
    ] {
        let rpc = parse_incoming(raw).expect("BUG: valid bool response must parse");
        let response = match rpc {
            Rpc::Response(response) => response,
            Rpc::Notification(_) => panic!("expected Response"),
        };
        assert_eq!(response.id, expected_id);
        let body = response
            .stratum_result
            .expect("BUG: result present in fixture");
        assert!(parse_boolean_result(body).expect("BUG: bool body must parse"));
    }
}

#[test]
fn parse_result_bool_large_id() {
    let line = br#"{"id":32769,"error":null,"result":true}"#;
    let rpc = parse_incoming(line).expect("BUG: valid response must parse");
    match rpc {
        Rpc::Response(response) => assert_eq!(response.id, 32769),
        Rpc::Notification(_) => panic!("expected Response"),
    }
}

#[test]
fn parse_result_bool_larger_id() {
    let line = br#"{"id":65536,"error":null,"result":true}"#;
    let rpc = parse_incoming(line).expect("BUG: valid response must parse");
    match rpc {
        Rpc::Response(response) => assert_eq!(response.id, 65536),
        Rpc::Notification(_) => panic!("expected Response"),
    }
}

#[test]
fn parse_error_array_with_empty_detail() {
    let line = br#"{"id":4,"result":null,"error":[21,"Job not found",""]}"#;
    let rpc = parse_incoming(line).expect("BUG: valid error response must parse");
    let response = match rpc {
        Rpc::Response(response) => response,
        Rpc::Notification(_) => panic!("expected Response"),
    };
    assert_eq!(response.id, 4);
    let error = response
        .stratum_error
        .expect("BUG: error present in fixture");
    assert_eq!(error.0, 21);
    assert_eq!(error.1, "Job not found");
    assert_eq!(error.2, Some(""));
}

#[test]
fn notify_dispatches() {
    let line = br#"{"id":null,"method":"mining.notify","params":["j1","aabb","cc","dd",[],"00000020","1a1b1c1d","5f0d4e2f",false]}"#;
    let rpc = parse_incoming(line).expect("BUG: valid notify line must parse");
    match rpc {
        Rpc::Notification(Notification::Notify(notify)) => {
            assert_eq!(notify.job_id, "j1");
            assert!(!notify.clean_jobs);
        }
        Rpc::Response(_) | Rpc::Notification(_) => {
            panic!("expected Notify notification")
        }
    }
}

#[test]
fn response_with_id() {
    let line = br#"{"id":7,"result":true,"error":null}"#;
    let rpc = parse_incoming(line).expect("BUG: valid response line must parse");
    match rpc {
        Rpc::Response(response) => assert_eq!(response.id, 7),
        Rpc::Notification(_) => panic!("expected Response"),
    }
}

#[test]
fn set_extranonce_dispatches() {
    let line = br#"{"id":null,"method":"mining.set_extranonce","params":["abcd1234",4]}"#;
    let rpc = parse_incoming(line).expect("BUG: valid set_extranonce line must parse");
    match rpc {
        Rpc::Notification(Notification::SetExtranonce(msg)) => {
            assert_eq!(msg.extra_nonce_1, "abcd1234");
            assert_eq!(msg.extra_nonce_2_size, 4);
        }
        _ => panic!("expected SetExtranonce notification"),
    }
}

#[test]
fn ping_dispatches() {
    let line = br#"{"id":42,"method":"mining.ping","params":[]}"#;
    let rpc = parse_incoming(line).expect("BUG: valid ping line must parse");
    match rpc {
        Rpc::Notification(Notification::Ping { id }) => assert_eq!(id, 42),
        _ => panic!("expected Ping notification"),
    }
}

#[test]
fn client_reconnect_dispatches() {
    let line = br#"{"id":null,"method":"client.reconnect","params":["pool.example",3333,0]}"#;
    let rpc = parse_incoming(line).expect("BUG: valid client.reconnect line must parse");
    match rpc {
        Rpc::Notification(Notification::ClientReconnect(msg)) => {
            assert_eq!(msg.host, "pool.example");
            assert_eq!(msg.port, 3333);
            assert_eq!(msg.wait, 0);
        }
        _ => panic!("expected ClientReconnect notification"),
    }
}

#[test]
fn unknown_server_method_is_error() {
    let line = br#"{"id":null,"method":"mining.bogus","params":[]}"#;
    match parse_incoming(line) {
        Err(ParseError::UnknownMethod("mining.bogus")) => {}
        other => panic!("expected UnknownMethod, got {:?}", other),
    }
}

#[test]
fn version_rolling_mask_extracted_from_configure_result() {
    // Public pool wire shape: flat object with `version-rolling` + dotted
    // `version-rolling.mask` keys.
    let body = br#"{"version-rolling":true,"version-rolling.mask":"1fffe000"}"#;
    assert_eq!(parse_version_rolling_mask(body), Some(0x1fff_e000));
}

#[test]
fn version_rolling_mask_absent_when_feature_rejected() {
    let body = br#"{"version-rolling":false}"#;
    assert_eq!(parse_version_rolling_mask(body), None);
}

#[test]
fn version_rolling_mask_rejects_non_hex() {
    let body = br#"{"version-rolling.mask":"zzzzzzzz"}"#;
    assert_eq!(parse_version_rolling_mask(body), None);
}
