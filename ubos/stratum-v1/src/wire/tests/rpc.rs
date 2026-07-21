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

//! Tests for `rpc`.

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
