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
//! The rules run on a synthetic chain of two versions, so they hold
//! whatever the real list holds: version 1 holds the API login alone,
//! version 2 adds the hostname, and the live [`Config`] follows version 2.
//! The real list is tested on the records of its versions.

use serde::{Deserialize, Serialize};

use crate::network::HOSTNAME_MAX;
use crate::tests_fixture::default_config;
use crate::versions::{
    MIN_SCHEMA_PREFIX_LEN, SchemaPrefix, Step, VERSIONS, Version, Versioned, downgrade_version,
    older_version,
};
use crate::{ApiConfig, Config, LOAD_BUFFER_SIZE, NetworkConfig, StagedConfig, decode_blob};

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
            ..default_config()
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
    let mut config = default_config();
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

/// A staged image that declares the whole of `hash`.
fn whole(hash: [u8; 8]) -> StagedConfig {
    StagedConfig::Schema(SchemaPrefix::new(hash))
}

/// The version a downgrade stores, or `None` when it stores nothing.
fn downgrade(staged: StagedConfig, versions: &[Version]) -> Option<u16> {
    downgrade_version(staged, versions).map(|version| version.number)
}

/// A build stores the record of an older version that the staged image
/// declares.
#[test]
fn downgrade_to_an_older_version_stores_its_record() {
    assert_eq!(downgrade(whole(V1::HASH), CHAIN), Some(1));
}

/// A staged image of the build's own schema, or of one the build does
/// not know, reads the stored record as it is.
#[test]
fn downgrade_to_the_own_or_an_unknown_schema_stores_nothing() {
    assert_eq!(downgrade(whole(V2::HASH), CHAIN), None);
    assert_eq!(downgrade(whole(UNKNOWN), CHAIN), None);
}

/// An unrecognized image takes the record of the oldest version, so a
/// ubos image installed after it upgrades from that record, and nothing
/// when the oldest version is the build's own.
#[test]
fn downgrade_to_an_unrecognized_image_stores_the_oldest_version() {
    assert_eq!(downgrade(StagedConfig::Unrecognized, CHAIN), Some(1));
    assert_eq!(downgrade(StagedConfig::Unrecognized, &CHAIN[..1]), None);
    assert_eq!(downgrade(StagedConfig::Unrecognized, &[]), None);
}

/// A staged image that declares the first `N` bytes of `hash`.
fn prefix<const N: usize>(hash: [u8; 8]) -> StagedConfig {
    StagedConfig::Schema(SchemaPrefix::new::<N>(
        *hash.first_chunk().expect("BUG: a hash holds the prefix"),
    ))
}

/// A prefix of any length the header format chooses takes the older
/// version whose hash starts with it, and nothing for the build's own
/// schema or for one the build does not know.
#[test]
fn downgrade_to_a_prefix_stores_the_version_it_starts() {
    assert_eq!(
        downgrade(prefix::<MIN_SCHEMA_PREFIX_LEN>(V1::HASH), CHAIN),
        Some(1)
    );
    assert_eq!(downgrade(prefix::<5>(V1::HASH), CHAIN), Some(1));
    assert_eq!(downgrade(prefix::<5>(V2::HASH), CHAIN), None);
    assert_eq!(downgrade(prefix::<5>(UNKNOWN), CHAIN), None);
}

/// No two versions in the real list share their first
/// [`MIN_SCHEMA_PREFIX_LEN`] bytes, so a prefix of any allowed length
/// names one version.
#[test]
fn versions_have_distinct_prefixes() {
    for (index, version) in VERSIONS.iter().enumerate() {
        assert!(
            VERSIONS[..index]
                .iter()
                .all(|older| older.hash[..MIN_SCHEMA_PREFIX_LEN]
                    != version.hash[..MIN_SCHEMA_PREFIX_LEN]),
            "version {} shares its schema hash prefix with an older version",
            version.number
        );
    }
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
        ..default_config()
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
        ..default_config()
    };
    assert_eq!((CHAIN[1].upgrade)(&buffer[8..length]), Some(expected));

    let length = (CHAIN[0].downgrade)(&config, &mut buffer).expect("BUG: the record fits");
    let expected = Config {
        api: config.api,
        ..default_config()
    };
    assert_eq!((CHAIN[0].upgrade)(&buffer[8..length]), Some(expected));
}

/// A record of version 1: the BMM101 factory defaults, with the
/// derivative smoother's half-decay at 7 seconds.
const VERSION_1_RECORD: &[u8] = &[
    22, 94, 167, 65, 122, 95, 186, 108, // schema hash of version 1
    1,   // pool: length 1
    19, 115, 116, 114, 97, 116, 117, 109, 46, 98, 114, 97, 105, 105, 110, 115, 46, 99, 111,
    109, // pool[0].host = "stratum.braiins.com"
    133, 26, // pool[0].port = 3333
    0,  // pool[0].user = ""
    1,  // pool[0].protocol = Sv2
    1,  // pool[0].protocol.pool_pubkey = Some
    41, 149, 76, 151, 69, 107, 89, 156, 131, 111, 112, 245, 8, 214, 25, 211, 40, 82, 123, 184, 114,
    41, 215, 215, 210, 230, 126, 109, 205, 24, 235, 207, // the default pool key
    0,   // pool[0].weight = 0
    232, 7, // thermal.sample_interval_ms = 1000
    1, 240, 144, 10, // thermal.dangerous_hashboard_temp = Some(83000)
    0,  // thermal.dangerous_chip_temp = None
    144, 191, 5, // thermal.cooldown_temp = 45000
    0, // cooling.mode = Auto
    0, // cooling.target.source = Hashboard
    0, // cooling.target.statistic = Max
    192, 169, 7,  // cooling.target.temp = 60000
    50, // cooling.fixed_fan_pwm = 50
    20, // cooling.pid.min_fan_pwm = 20
    0, 0, 0, 0, 0, 0, 20, 192, // cooling.pid.p_gain = -5.0
    51, 51, 51, 51, 51, 51, 195, 191, // cooling.pid.i_gain = -0.15
    0, 0, 0, 0, 0, 0, 89, 192, // cooling.pid.d_gain = -100.0
    7,   // cooling.pid.smoother_half_decay_secs = 7 (varint)
    0,   // cooling.pid.input_smoother_half_decay_secs = 0
    0,   // miner.tuning = Fixed
    200, 133, 32, // miner.tuning.Fixed.frequency_khz = 525000
    248, 10, // miner.tuning.Fixed.voltage_mv = 1400
    4, 114, 111, 111, 116, // api.username = "root"
    4, 114, 111, 111, 116, // api.password = "root"
    0,   // network.mode = Dhcp
    0,   // network.hostname = ""
];

/// The record of version 1 loads as the live config, so an edit to the
/// schema or the hash of version 1 fails here.
#[test]
fn version_1_record_loads_as_the_live_config() {
    assert!(
        option_env!("POOL_HOST").is_none()
            && option_env!("POOL_PORT").is_none()
            && option_env!("POOL_USER").is_none(),
        "POOL_* env vars override the default pool; unset them to run this test",
    );
    let mut expected = default_config();
    expected.cooling.pid.smoother_half_decay_secs = 7;
    assert_eq!(decode_blob::<()>(VERSION_1_RECORD).ok(), Some(expected));
}
