//! V2-specific session config; the firmware passes a `&'static`
//! reference into `StratumV2Client::new`.

/// Pool credentials and authentication material for a Stratum V2
/// session. `user` becomes the channel `user_identity` field on
/// `OpenStandardMiningChannel`. `pool_pubkey`, when present, is
/// used for the noise handshake's `ValidateSignature` authenticator;
/// `None` falls back to `ValidateNothing` for unauthenticated pool
/// connections (test environments only).
pub struct Sv2Config {
    pub user: &'static str,
    pub pool_pubkey: Option<[u8; 32]>,
}
