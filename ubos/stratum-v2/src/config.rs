//! V2-specific session config. Constructed once by the caller (a
//! firmware boot path or a host probe) and handed to
//! `StratumV2Client::new` by reference.

use heapless::String;

#[cfg(test)]
#[path = "tests/config.rs"]
mod tests;

/// Maximum byte length of the `user_identity` field. A typical V2
/// worker identity is `<bech32 address>.<worker name>`; a P2TR
/// address tops out at 62 chars, plus `.<workername>` of up to ~16
/// chars, comfortably under 128 bytes.
pub const MAX_USER_LEN: usize = 128;

/// `Sv2Config::new` rejects a worker identity that does not fit in
/// the fixed-size `user` buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserTooLong;

/// Pool credentials and authentication material for a Stratum V2
/// session. `user` becomes the channel `user_identity` field on
/// `OpenStandardMiningChannel`. `pool_pubkey`, when present, is
/// used for the noise handshake's `ValidateSignature` authenticator;
/// `None` falls back to `ValidateNothing` for unauthenticated pool
/// connections (test environments only).
///
/// `user` is owned (`heapless::String<128>`) rather than a `&str`
/// so the config has no borrow on its source string; callers can
/// hold the config in a `StaticCell` or pass by reference without
/// fighting `'static` lifetimes for the worker identity.
pub struct Sv2Config {
    pub user: String<MAX_USER_LEN>,
    pub pool_pubkey: Option<[u8; 32]>,
}

impl Sv2Config {
    /// Construct an `Sv2Config` from a `&str` worker identity.
    ///
    /// # Errors
    ///
    /// Returns `Err(UserTooLong)` if `user` is longer than
    /// [`MAX_USER_LEN`] bytes.
    pub fn new(user: &str, pool_pubkey: Option<[u8; 32]>) -> Result<Self, UserTooLong> {
        let mut owned = String::new();
        owned.push_str(user).map_err(|_| UserTooLong)?;
        Ok(Self {
            user: owned,
            pool_pubkey,
        })
    }
}
