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

//! Tests for the version list and the steps between config versions.
//!
//! No version is listed yet, so these build a synthetic chain of two
//! versions: version 1 holds the API login alone, version 2 adds the
//! hostname, and the live [`Config`] follows version 2.

use serde::{Deserialize, Serialize};

use crate::network::HOSTNAME_MAX;
use crate::versions::{Step, Version, Versioned, older_version};
use crate::{ApiConfig, Config, LOAD_BUFFER_SIZE, NetworkConfig};

/// Version 1 of the synthetic chain: the API login alone.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct V1 {
    api: ApiConfig,
}

/// Version 2 of the synthetic chain: the login and the hostname.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct V2 {
    api: ApiConfig,
    hostname: heapless::String<HOSTNAME_MAX>,
}

impl Versioned for V1 {
    const HASH: [u8; 8] = [1; 8];
}

impl Versioned for V2 {
    const HASH: [u8; 8] = [2; 8];
}

impl Step for V1 {
    type Next = V2;

    fn up(self) -> V2 {
        V2 {
            api: self.api,
            hostname: heapless::String::new(),
        }
    }

    fn down(next: &V2) -> Self {
        Self {
            api: next.api.clone(),
        }
    }
}

impl Step for V2 {
    type Next = Config;

    fn up(self) -> Config {
        Config {
            api: self.api,
            network: NetworkConfig {
                hostname: self.hostname,
                ..NetworkConfig::default()
            },
            ..Config::default()
        }
    }

    fn down(next: &Config) -> Self {
        Self {
            api: next.api.clone(),
            hostname: next.network.hostname.clone(),
        }
    }
}

/// The synthetic version list, oldest first. Its newest entry stands for
/// the build's own version.
const CHAIN: &[Version] = &[Version::of::<V1>(1), Version::of::<V2>(2)];

/// A hash that no version in the chain has.
const UNKNOWN: [u8; 8] = [9; 8];

/// A config that differs from the defaults in every field of the chain
/// and in a field that no version of it holds.
fn edited() -> Config {
    let mut config = Config::default();
    config.api.username = "operator".try_into().expect("BUG: username fits");
    config.network.hostname = "miner-1".try_into().expect("BUG: hostname fits");
    config.thermal.sample_interval_ms += 1;
    config
}

/// The number of the version older than the newest of `versions` whose
/// schema is `hash`, or `None` when there is none.
fn older(hash: [u8; 8], versions: &[Version]) -> Option<u16> {
    older_version(hash, versions).map(|version| version.number)
}

/// The build's own version is no older version.
#[test]
fn own_version_is_no_older_version() {
    assert_eq!(older(V2::HASH, CHAIN), None);
}

/// A build upgrades the record of an older version.
#[test]
fn build_upgrades_an_older_version() {
    assert_eq!(older(V1::HASH, CHAIN), Some(1));
}

/// A build cannot read the record of a version newer than its own.
#[test]
fn newer_version_is_lost() {
    assert_eq!(older(V2::HASH, &CHAIN[..1]), None);
}

/// A record whose hash no version has is lost.
#[test]
fn unknown_hash_is_lost() {
    assert_eq!(older(UNKNOWN, CHAIN), None);
}

/// An empty list reads no older version.
#[test]
fn empty_list_reads_no_older_version() {
    assert_eq!(older(V1::HASH, &[]), None);
}

/// A record of version 1 goes up both steps to the live config, and the
/// fields version 1 lacks take their defaults.
#[test]
fn oldest_record_upgrades_to_the_live_config() {
    let login = edited().api;
    let mut buffer = [0u8; LOAD_BUFFER_SIZE];
    let payload =
        postcard::to_slice(&V1 { api: login.clone() }, &mut buffer).expect("BUG: V1 encodes");
    let expected = Config {
        api: login,
        ..Config::default()
    };
    assert_eq!((CHAIN[0].upgrade)(payload), Some(expected));
}

/// A payload that does not decode as its version gives no config.
#[test]
fn damaged_payload_does_not_upgrade() {
    assert_eq!((CHAIN[1].upgrade)(&[0xFF]), None);
}

/// The live config goes down both steps to a record of version 1, framed
/// by the hash of version 1.
#[test]
fn live_config_downgrades_to_the_oldest_record() {
    let config = edited();
    let mut buffer = [0u8; LOAD_BUFFER_SIZE];
    let length = (CHAIN[0].downgrade)(&config, &mut buffer).expect("BUG: the record fits");
    let (hash, payload) = buffer[..length]
        .split_first_chunk::<8>()
        .expect("BUG: the record has a hash");
    assert_eq!(*hash, V1::HASH);
    assert_eq!(
        postcard::from_bytes::<V1>(payload).expect("BUG: V1 decodes"),
        V1 { api: config.api }
    );
}

/// Down to a version and up again gives the original config, with the
/// fields that version lacks at their defaults.
#[test]
fn down_then_up_keeps_what_the_version_holds() {
    let config = edited();
    let mut buffer = [0u8; LOAD_BUFFER_SIZE];

    let length = (CHAIN[1].downgrade)(&config, &mut buffer).expect("BUG: the record fits");
    let expected = Config {
        api: config.api.clone(),
        network: config.network.clone(),
        ..Config::default()
    };
    assert_eq!((CHAIN[1].upgrade)(&buffer[8..length]), Some(expected));

    let length = (CHAIN[0].downgrade)(&config, &mut buffer).expect("BUG: the record fits");
    let expected = Config {
        api: config.api,
        ..Config::default()
    };
    assert_eq!((CHAIN[0].upgrade)(&buffer[8..length]), Some(expected));
}
