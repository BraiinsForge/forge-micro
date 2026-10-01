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

//! Tests for the JSON schemas derived behind the `schema` feature.

extern crate std;
use std::vec::Vec;

use crate::{ConfigUpdate, NAME_CAPACITY, PoolEntry};

// The pool entry schema states the host and worker name capacity, so
// a client rejects an over-long name before the device does.
#[test]
fn pool_entry_schema_states_the_capacity() {
    let schema = schemars::schema_for!(PoolEntry);
    let value = serde_json::to_value(&schema).expect("schema is JSON");
    let properties = value["properties"].as_object().expect("root is an object");
    assert_eq!(properties["host"]["maxLength"], NAME_CAPACITY);
    assert_eq!(properties["user"]["maxLength"], NAME_CAPACITY);
}

// The config schema names every section and keeps the tagged
// tuning shape, so a client generator reads the same keys the
// device accepts.
#[test]
fn config_update_schema_names_every_section() {
    let schema = schemars::schema_for!(ConfigUpdate);
    let value = serde_json::to_value(&schema).expect("schema is JSON");
    let properties = value["properties"].as_object().expect("root is an object");
    for key in ["pool", "thermal", "cooling", "miner", "api", "network"] {
        assert!(properties.contains_key(key), "missing section {key}");
    }
    let tuning = &value["$defs"]["Tuning"];
    let variants: Vec<&str> = tuning["oneOf"]
        .as_array()
        .expect("Tuning is a oneOf")
        .iter()
        .map(|v| v["required"][0].as_str().expect("variant names its key"))
        .collect();
    assert_eq!(variants, ["Fixed", "Power"]);
}
