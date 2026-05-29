//! Unit tests for blob framing and storage error paths.

use crate::{decode_blob, encode_blob, Config, LoadError, DEFAULT_POOL_SV2_PUBKEY};

/// Base58check literal whose decoded 32-byte tail must equal
/// [`DEFAULT_POOL_SV2_PUBKEY`]. Kept in sync with the doc comment on
/// the constant.
const DEFAULT_POOL_SV2_PUBKEY_BASE58: &str = "9awtMD5KQgvRUh2yFbjVeT7b6hjipWcAsQHd6wEhgtDT9soosna";

/// SV2 authority key prefix expected by the original `pool_config`
/// decoder; preserved here so a future encoding change to the literal
/// is caught alongside the byte sequence.
const SV2_PUBKEY_PREFIX: [u8; 2] = [1, 0];

/// Round-trip: encode the default config and decode it back.
#[test]
fn round_trip_default() {
    let config = Config::default();
    let mut buffer = [0u8; 512];
    let length = encode_blob(&config, &mut buffer).expect("encode must succeed");
    let decoded: Config = decode_blob::<()>(&buffer[..length]).expect("decode must succeed");
    assert_eq!(decoded, config);
}

/// A blob with a wrong 8-byte prefix returns SchemaMismatch.
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
#[test]
fn golden_snapshot() {
    assert!(
        option_env!("POOL_HOST").is_none()
            && option_env!("POOL_PORT").is_none()
            && option_env!("POOL_USER").is_none(),
        "POOL_* env vars override Config::default; unset them to run this test",
    );

    // Schema hash for ubos_config::Config as of this commit.
    // Update this when Config's shape changes.
    const EXPECTED_HASH: [u8; 8] = [15, 25, 189, 233, 212, 18, 114, 221];

    // Postcard encoding of Config::default() with the schema-hash prefix.
    // 111 bytes total: 8-byte hash + 103-byte payload.
    // Update this when Config's default values or shape changes.
    const EXPECTED_BLOB: &[u8] = &[
        15, 25, 189, 233, 212, 18, 114, 221, // schema hash
        1,   // pool: heapless::Vec<PoolEntry, 3> (length=1)
        19, 115, 116, 114, 97, 116, 117, 109, 46, 98, 114, 97, 105, 105, 110, 115, 46, 99, 111,
        109, // pool[0].host = "stratum.braiins.com" (len 19)
        133, 26, // pool[0].port = 3333 (varint)
        0,  // pool[0].user = "" (len 0)
        1,  // pool[0].protocol tag = 1 (Sv2)
        1,  // pool[0].protocol.pool_pubkey = Some
        // pool[0].protocol.pool_pubkey = DEFAULT_POOL_SV2_PUBKEY (32 bytes)
        0x29, 0x95, 0x4c, 0x97, 0x45, 0x6b, 0x59, 0x9c, 0x83, 0x6f, 0x70, 0xf5, 0x08, 0xd6, 0x19,
        0xd3, 0x28, 0x52, 0x7b, 0xb8, 0x72, 0x29, 0xd7, 0xd7, 0xd2, 0xe6, 0x7e, 0x6d, 0xcd, 0x18,
        0xeb, 0xcf, //
        232, 7, // thermal.sample_interval_ms = 1000 (varint)
        0, 0, 180, 66, // thermal.dangerous_temp_c = 90.0f32
        0, 0, 52, 66, // thermal.cooldown_temp_c = 45.0f32
        0, 0, 134, 66, // cooling.target_temp_c = 67.0f32
        20, // cooling.pid.min_fan_pwm = 20 (varint)
        0, 0, 160, 192, // cooling.pid.p_gain = -5.0f32
        154, 153, 25, 190, // cooling.pid.i_gain = -0.15f32
        0, 0, 200, 194, // cooling.pid.d_gain = -100.0f32
        0, 0, 32, 65, // cooling.pid.smoother_half_decay_secs = 10.0f32
        141, 4, // tuning.frequency_mhz = 525 (varint)
        248, 10, // tuning.voltage_mv = 1400 (varint)
        4, 114, 111, 111, 116, // api.username = "root"
        4, 114, 111, 111, 116, // api.password = "root"
    ];

    assert_eq!(Config::SCHEMA_HASH, EXPECTED_HASH);

    let config = Config::default();
    let mut buffer = [0u8; 512];
    let length = encode_blob(&config, &mut buffer).expect("encode must succeed");
    assert_eq!(&buffer[..length], EXPECTED_BLOB);
}

/// Decoding the base58check literal embedded in the
/// [`DEFAULT_POOL_SV2_PUBKEY`] doc comment must produce the hardcoded
/// 32-byte constant. Catches typos in the literal or the bytes if
/// either side is edited without the other.
#[test]
fn default_pool_sv2_pubkey_matches_base58check_literal() {
    let mut buffer = [0u8; 64];
    let length = bs58::decode(DEFAULT_POOL_SV2_PUBKEY_BASE58)
        .with_check(None)
        .onto(&mut buffer[..])
        .expect("base58check decode must succeed");
    assert_eq!(length, 34, "expected 2-byte prefix + 32-byte key");
    assert_eq!(&buffer[..2], &SV2_PUBKEY_PREFIX);
    assert_eq!(&buffer[2..34], &DEFAULT_POOL_SV2_PUBKEY);
}
