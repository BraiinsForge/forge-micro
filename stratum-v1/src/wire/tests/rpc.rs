//! Tests for rpc.

use super::*;
use crate::wire::parse::parse_incoming;

#[test]
fn response_ok() {
    let line = br#"{"id":1,"result":true,"error":null}"#;
    let rpc = parse_incoming(line).expect("BUG: valid response line must parse");
    match rpc {
        Rpc::Response(response) => {
            assert_eq!(response.id, 1);
            assert!(response.stratum_result.is_some());
            assert!(response.stratum_error.is_none());
        }
        Rpc::Notification(_) => panic!("expected Response"),
    }
}

#[test]
fn response_error() {
    let line = br#"{"id":2,"result":null,"error":[21,"Job not found",null]}"#;
    let rpc = parse_incoming(line).expect("BUG: valid response line must parse");
    match rpc {
        Rpc::Response(response) => {
            assert_eq!(response.id, 2);
            assert!(response.stratum_error.is_some());
            let error = response
                .stratum_error
                .expect("BUG: stratum_error was just asserted Some");
            assert_eq!(error.0, 21);
            assert_eq!(error.1, "Job not found");
        }
        Rpc::Notification(_) => panic!("expected Response"),
    }
}

#[test]
fn stratum_error_tuple_shape() {
    let line = br#"{"id":3,"result":null,"error":[20,"Other error","detail"]}"#;
    let rpc = parse_incoming(line).expect("BUG: valid response line must parse");
    match rpc {
        Rpc::Response(response) => {
            let error = response
                .stratum_error
                .expect("BUG: error field present in input");
            assert_eq!(error.0, 20);
            assert_eq!(error.1, "Other error");
            assert_eq!(error.2, Some("detail"));
        }
        Rpc::Notification(_) => panic!("expected Response"),
    }
}
