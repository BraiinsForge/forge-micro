use super::*;

const DEFAULT_USER: &str = "bcrt1qsdtjq8zjhefamsumwcfc6yh4gd98c900v8r4e8.test";

#[test]
fn parse_defaults_yields_sv1_against_braiins_with_no_password() {
    let config = parse(
        DEFAULT_POOL_HOST,
        DEFAULT_POOL_PORT_STR,
        DEFAULT_POOL_PROTOCOL,
        DEFAULT_USER,
        None,
        None,
    )
    .expect("parse defaults");
    assert_eq!(config.host.as_str(), "stratum.braiins.com");
    assert_eq!(config.port, 3333);
    assert!(matches!(
        config.protocol,
        ProtocolConfig::Sv1 { password: None }
    ));
}

#[test]
fn parse_sv1_with_explicit_password() {
    let config = parse("h", "3333", "sv1", "u", Some("x"), None).expect("parse sv1+pass");
    let ProtocolConfig::Sv1 {
        password: Some(pass),
    } = config.protocol
    else {
        panic!("expected Sv1 with Some password");
    };
    assert_eq!(pass.as_str(), "x");
}

#[test]
fn parse_sv2_with_no_pubkey_yields_validate_nothing() {
    let config = parse("pool.example", "34254", "sv2", "user.worker", None, None).expect("parse");
    assert!(matches!(
        config.protocol,
        ProtocolConfig::Sv2 { pool_pubkey: None }
    ));
}

#[test]
fn parse_sv2_decodes_base58check_pubkey() {
    // Encode a known 32-byte payload with the SV2 [1, 0] version prefix
    // and assert decode returns exactly those bytes.
    let known_key = [0xABu8; 32];
    let mut raw = [0u8; 34];
    raw[..2].copy_from_slice(&[1, 0]);
    raw[2..].copy_from_slice(&known_key);
    let encoded = bs58::encode(raw).with_check().into_string();

    let config = parse(
        "pool.example",
        "34254",
        "sv2",
        "user.worker",
        None,
        Some(&encoded),
    )
    .expect("parse");
    let ProtocolConfig::Sv2 {
        pool_pubkey: Some(key),
    } = config.protocol
    else {
        panic!("expected Sv2 with Some pubkey");
    };
    assert_eq!(key, known_key);
}

#[test]
fn parse_rejects_unknown_protocol() {
    let err = parse("h", "1", "stratum3", "u", None, None).expect_err("expected ConfigError");
    assert_eq!(err, ConfigError::InvalidProtocol);
}

#[test]
fn parse_rejects_non_numeric_port() {
    let err = parse("h", "thirty-three-thirty-three", "sv1", "u", None, None)
        .expect_err("expected ConfigError");
    assert_eq!(err, ConfigError::InvalidPort);
}

#[test]
fn parse_rejects_port_overflow() {
    let err = parse("h", "65536", "sv1", "u", None, None).expect_err("expected ConfigError");
    assert_eq!(err, ConfigError::InvalidPort);
}

#[test]
fn parse_rejects_oversized_host() {
    let long = "a".repeat(MAX_HOST_LEN + 1);
    let err = parse(&long, "3333", "sv1", "u", None, None).expect_err("expected ConfigError");
    assert_eq!(err, ConfigError::HostTooLong);
}

#[test]
fn parse_rejects_oversized_user() {
    let long = "a".repeat(MAX_USER_LEN + 1);
    let err = parse("h", "3333", "sv1", &long, None, None).expect_err("expected ConfigError");
    assert_eq!(err, ConfigError::UserTooLong);
}

#[test]
fn parse_rejects_oversized_sv1_pass() {
    let long = "a".repeat(MAX_PASS_LEN + 1);
    let err = parse("h", "3333", "sv1", "u", Some(&long), None).expect_err("expected ConfigError");
    assert_eq!(err, ConfigError::PassTooLong);
}

#[test]
fn parse_rejects_garbage_pubkey() {
    let err = parse("h", "3333", "sv2", "u", None, Some("not-base58-check!"))
        .expect_err("expected ConfigError");
    assert_eq!(err, ConfigError::InvalidPubkey);
}

#[test]
fn parse_rejects_sv2_pubkey_wrong_version_prefix() {
    // Plain 32-byte base58check without the SV2 `[1, 0]` prefix is
    // a valid base58check string but not a valid SV2 PublicKey.
    let raw = [0u8; 32];
    let plain = bs58::encode(raw).with_check().into_string();
    let err = parse("h", "3333", "sv2", "u", None, Some(&plain)).expect_err("expected ConfigError");
    assert_eq!(err, ConfigError::InvalidPubkey);
}
