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

//! Unit tests for blob framing and storage error paths.

use crate::{Config, LoadError, NetworkConfig, NetworkMode, decode_blob, encode_blob};

/// Round-trip: encode the default config and decode it back.
#[test]
fn round_trip_default() {
    let config = Config::default();
    let mut buffer = [0u8; 512];
    let length = encode_blob(&config, &mut buffer).expect("encode must succeed");
    let decoded: Config = decode_blob::<()>(&buffer[..length]).expect("decode must succeed");
    assert_eq!(decoded, config);
}

/// Round-trip a static network config so the encoding of the
/// `NetworkMode::Static` variant and its address/prefix/gateway/dns
/// fields plus a non-empty hostname is exercised, not just the DHCP
/// default the other tests cover.
#[test]
fn round_trip_static_network() {
    let mut dns = heapless::Vec::new();
    dns.push([1, 1, 1, 1]).expect("dns slot available");
    dns.push([8, 8, 8, 8]).expect("dns slot available");
    let config = Config {
        network: NetworkConfig {
            mode: NetworkMode::Static {
                address: [192, 168, 1, 50],
                prefix_len: 24,
                gateway: [192, 168, 1, 1],
                dns,
            },
            hostname: heapless::String::try_from("miner-1").expect("BUG: hostname fits"),
        },
        ..Default::default()
    };
    let mut buffer = [0u8; 512];
    let length = encode_blob(&config, &mut buffer).expect("encode must succeed");
    let decoded: Config = decode_blob::<()>(&buffer[..length]).expect("decode must succeed");
    assert_eq!(decoded, config);
}

/// A blob with a wrong 8-byte prefix returns `SchemaMismatch`.
#[test]
fn schema_mismatch_returns_error() {
    // All zeroes: wrong prefix, rest unused.
    let blob = [0u8; 32];
    let result = decode_blob::<()>(&blob);
    match result {
        Err(LoadError::SchemaMismatch { stored, expected }) => {
            assert_eq!(stored, [0u8; 8]);
            assert_eq!(expected, Config::SCHEMA_HASH);
        }
        other => panic!("expected SchemaMismatch, got {other:?}"),
    }
}

/// A valid schema-hash prefix but a corrupted postcard payload returns Corrupt.
#[test]
fn corrupted_payload_returns_corrupt() {
    let config = Config::default();
    let mut buffer = [0u8; 512];
    let length = encode_blob(&config, &mut buffer).expect("encode must succeed");
    // Flip a byte in the postcard payload (offset 8 and beyond).
    if length > 8 {
        buffer[8] ^= 0xFF;
    }
    let result = decode_blob::<()>(&buffer[..length]);
    assert!(
        matches!(result, Err(LoadError::Corrupt)),
        "expected Corrupt, got {result:?}"
    );
}

/// Golden snapshot guards the schema hash and the default-config encoding.
///
/// These hardcoded values were captured on first run. Any change to
/// `Config`'s field set, field order, field types, or type names shifts
/// the schema hash, breaks this test, and forces the developer to
/// consciously update both the expected hash and the golden bytes before
/// the commit can land.
///
/// Precondition: the `POOL_*` env vars consumed by [`Config::default`]
/// must be unset at build time. The golden bytes capture the unmodified
/// factory defaults; setting any override shifts the encoded payload.
// Schema hash for ubos_config::Config as of this commit.
// Update this when Config's shape changes.
const EXPECTED_HASH: [u8; 8] = [110, 154, 55, 219, 96, 10, 181, 17];

// Postcard encoding of Config::default() with the schema-hash prefix.
// 122 bytes total: 8-byte hash + 112-byte pre-network payload + 2-byte
// network section.
// Update this when Config's default values or shape changes.
const EXPECTED_BLOB: &[u8] = &[
    110, 154, 55, 219, 96, 10, 181, 17, // schema hash
    1,  // pool: heapless::Vec<PoolEntry, 3> (length=1)
    19, 115, 116, 114, 97, 116, 117, 109, 46, 98, 114, 97, 105, 105, 110, 115, 46, 99, 111,
    109, // pool[0].host = "stratum.braiins.com" (len 19)
    133, 26, // pool[0].port = 3333 (varint)
    0,  // pool[0].user = "" (len 0)
    1,  // pool[0].protocol tag = 1 (Sv2)
    1,  // pool[0].protocol.pool_pubkey = Some
    // pool[0].protocol.pool_pubkey = DEFAULT_POOL_SV2_PUBKEY (32 bytes)
    41, 149, 76, 151, 69, 107, 89, 156, 131, 111, 112, 245, 8, 214, 25, 211, 40, 82, 123, 184, 114,
    41, 215, 215, 210, 230, 126, 109, 205, 24, 235, 207, //
    0,   // pool[0].weight = 0 (varint)
    232, 7, // thermal.sample_interval_ms = 1000 (varint)
    240, 144, 10, // thermal.dangerous_hashboard_temp = 83000 (zigzag varint)
    224, 182, 13, // thermal.dangerous_chip_temp = 110000 (zigzag varint)
    144, 191, 5, // thermal.cooldown_temp = 45000 (zigzag varint)
    0, // cooling.mode = Auto (variant 0)
    192, 169, 7,  // cooling.target_temp = 60000 (zigzag varint)
    50, // cooling.fixed_fan_pwm = 50 (varint)
    20, // cooling.pid.min_fan_pwm = 20 (varint)
    0, 0, 160, 192, // cooling.pid.p_gain = -5.0f32
    154, 153, 25, 190, // cooling.pid.i_gain = -0.15f32
    0, 0, 200, 194, // cooling.pid.d_gain = -100.0f32
    0, 0, 32, 65, // cooling.pid.smoother_half_decay_secs = 10.0f32
    0, 0, 0, 0, // cooling.pid.input_smoother_half_decay_secs = 0.0f32
    0, // miner.tuning = Fixed (variant 0)
    200, 133, 32, // miner.tuning.Fixed.frequency_khz = 525000 (varint)
    248, 10, // miner.tuning.Fixed.voltage_mv = 1400 (varint)
    4, 114, 111, 111, 116, // api.username = "root"
    4, 114, 111, 111, 116, // api.password = "root"
    0,   // network.mode tag = 0 (Dhcp)
    0,   // network.hostname = "" (len 0)
];

// The golden bytes capture the BMM101 factory defaults, which is what
// `Config::default()` produces.
#[test]
fn golden_snapshot() {
    assert!(
        option_env!("POOL_HOST").is_none()
            && option_env!("POOL_PORT").is_none()
            && option_env!("POOL_USER").is_none(),
        "POOL_* env vars override Config::default; unset them to run this test",
    );

    assert_eq!(Config::SCHEMA_HASH, EXPECTED_HASH);

    let config = Config::default();
    let mut buffer = [0u8; 512];
    let length = encode_blob(&config, &mut buffer).expect("encode must succeed");
    assert_eq!(&buffer[..length], EXPECTED_BLOB);
}
