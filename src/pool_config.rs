//! Pool connection config driven by `option_env!` overrides.
//!
//! Each firmware reads six environment variables at compile time
//! (with fall-through defaults below) and hands the resulting
//! strings to [`parse`]. The returned [`PoolConfig`] feeds the
//! runtime protocol selector in the stratum task: matching on
//! [`ProtocolConfig`] picks `StratumV1Client` vs `StratumV2Client`
//! without a Cargo feature switch.
//!
//! ## Variables
//!
//! | Var | Type | Default | Notes |
//! |---|---|---|---|
//! | `POOL_HOST` | string | [`DEFAULT_POOL_HOST`] | IPv4 literal or DNS name |
//! | `POOL_PORT` | u16 | [`DEFAULT_POOL_PORT_STR`] | |
//! | `POOL_PROTOCOL` | `sv1`/`sv2` | [`DEFAULT_POOL_PROTOCOL`] | |
//! | `POOL_USER` | string | per-board | worker identity, P2TR + `.workersuffix` |
//! | `POOL_SV1_PASS` | string | unset → no password | ignored under `sv2` |
//! | `POOL_SV2_PUBKEY` | base58check string | unset → `ValidateNothing` | ignored under `sv1` |

use core::str::FromStr;

use heapless::String;

/// Default V1 ckpool on the lab NUC. Matches today's hardcoded
/// `Pool::HOST_IP` in firmware `config.rs`.
pub const DEFAULT_POOL_HOST: &str = "redacted100";

/// String form so the firmware's `option_env!` fall-through stays
/// uniform; [`parse`] runs `u16::from_str` once at boot.
pub const DEFAULT_POOL_PORT_STR: &str = "3333";

pub const DEFAULT_POOL_PROTOCOL: &str = "sv1";

/// Maximum byte length of `POOL_HOST` (DNS name or IPv4 literal)
/// and `POOL_USER`. Mining pool hostnames are short in practice;
/// 128 covers the longest reasonable cases.
pub const MAX_HOST_LEN: usize = 128;
pub const MAX_USER_LEN: usize = 128;
pub const MAX_PASS_LEN: usize = 32;

/// Parsed result of the six `POOL_*` env vars.
#[derive(Debug, Clone)]
pub struct PoolConfig {
    pub host: String<MAX_HOST_LEN>,
    pub port: u16,
    pub user: String<MAX_USER_LEN>,
    pub protocol: ProtocolConfig,
}

/// Protocol-specific subset of the pool config. The unused field
/// (V1 password under `sv2`, V2 pubkey under `sv1`) is simply
/// absent — there's no silent-ignore failure mode.
#[derive(Debug, Clone)]
pub enum ProtocolConfig {
    Sv1 {
        /// `None` → no `mining.authorize` password sent. Most
        /// pools authorise the worker by name alone; a pool that
        /// requires a password should reject the connect instead
        /// of having the firmware default to a guessable value.
        password: Option<String<MAX_PASS_LEN>>,
    },
    Sv2 {
        /// `None` → `ValidateNothing` (test pools); `Some` →
        /// `ValidateSignature` against the supplied authority key.
        pool_pubkey: Option<[u8; 32]>,
    },
}

/// Failure modes for [`parse`]. Each is a string-shape problem
/// caught at boot; the firmware should `expect` the result so a
/// misconfigured env shows up as a clear panic rather than a
/// hanging session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigError {
    HostTooLong,
    UserTooLong,
    PassTooLong,
    InvalidPort,
    InvalidProtocol,
    InvalidPubkey,
}

/// Parse the six raw env-var inputs into a typed [`PoolConfig`].
///
/// Callers (firmware crates) read each var via `option_env!`. For
/// the four required strings they apply the matching `DEFAULT_*`
/// constant as fall-through and pass a `&str`. For the two
/// optional fields (`POOL_SV1_PASS`, `POOL_SV2_PUBKEY`) they pass
/// the `Option<&str>` straight through — an unset env var is just
/// `None`, no sentinel string required. The function is
/// `no_std`-friendly and runs once at boot.
///
/// # Errors
///
/// See [`ConfigError`] for the exhaustive list. All variants are
/// recoverable in principle (the caller could substitute a default
/// and retry), but the firmware boot path treats any error as
/// fatal — a typo in `POOL_PUBKEY` is more useful as an immediate
/// panic than as a silent `ValidateNothing` fallback.
pub fn parse(
    host: &str,
    port_str: &str,
    protocol_str: &str,
    user: &str,
    sv1_pass: Option<&str>,
    sv2_pubkey_str: Option<&str>,
) -> Result<PoolConfig, ConfigError> {
    let host = String::<MAX_HOST_LEN>::from_str(host).map_err(|()| ConfigError::HostTooLong)?;
    let port: u16 = port_str.parse().map_err(|_| ConfigError::InvalidPort)?;
    let user = String::<MAX_USER_LEN>::from_str(user).map_err(|()| ConfigError::UserTooLong)?;
    let protocol = parse_protocol(protocol_str, sv1_pass, sv2_pubkey_str)?;

    Ok(PoolConfig {
        host,
        port,
        user,
        protocol,
    })
}

fn parse_protocol(
    protocol_str: &str,
    sv1_pass: Option<&str>,
    sv2_pubkey_str: Option<&str>,
) -> Result<ProtocolConfig, ConfigError> {
    match protocol_str {
        "sv1" => {
            let password: Option<String<MAX_PASS_LEN>> = sv1_pass
                .map(String::from_str)
                .transpose()
                .map_err(|()| ConfigError::PassTooLong)?;
            Ok(ProtocolConfig::Sv1 { password })
        }
        "sv2" => {
            let pool_pubkey = sv2_pubkey_str.map(decode_pubkey).transpose()?;
            Ok(ProtocolConfig::Sv2 { pool_pubkey })
        }
        _ => Err(ConfigError::InvalidProtocol),
    }
}

/// Decode the SV2-spec base58check public key into the raw 32-byte
/// xonly form. Mirrors what the host probe does via
/// `ii_sv2::security::PublicKey::from_str`, kept self-contained
/// here so this module does not need a direct dep on the V2 crates
/// (firmware builds that opt out of V2 still want
/// `pool_config::parse` to compile).
fn decode_pubkey(encoded: &str) -> Result<[u8; 32], ConfigError> {
    // Header: SV2 spec prefixes the 32-byte xonly key with `[1, 0]`
    // (little-endian version 1) before base58check encoding.
    const PUB_PREFIX: [u8; 2] = [1, 0];
    let mut buffer = [0u8; 64];
    let length = bs58::decode(encoded)
        .with_check(None)
        .onto(&mut buffer[..])
        .map_err(|_| ConfigError::InvalidPubkey)?;
    if length != 34 || buffer[..2] != PUB_PREFIX {
        return Err(ConfigError::InvalidPubkey);
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(&buffer[2..34]);
    Ok(key)
}

#[cfg(test)]
#[path = "tests/pool_config.rs"]
mod tests;
