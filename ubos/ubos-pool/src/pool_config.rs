//! Pool connection config driven by `option_env!` compile-time overrides.
//!
//! Setup: `../../../docs/SETUP.md`.

use core::str::FromStr;

use embassy_time::Duration;
use heapless::String;

/// Default SV2 authority public key for `stratum.braiins.com`.
/// Applied when `POOL_SV2_PUBKEY` is unset, so an unconfigured
/// firmware reaches the Braiins prod endpoint with peer
/// authentication enabled rather than `ValidateNothing`.
pub const DEFAULT_POOL_SV2_PUBKEY: &str = "9awtMD5KQgvRUh2yFbjVeT7b6hjipWcAsQHd6wEhgtDT9soosna";

/// Default Braiins pool endpoint. Firmware resolves the DNS name
/// at runtime via `stack.dns_query`.
pub const DEFAULT_POOL_HOST: &str = "stratum.braiins.com";

/// String form so the firmware's `option_env!` fall-through stays
/// uniform; [`parse`] runs `u16::from_str` once at boot.
pub const DEFAULT_POOL_PORT_STR: &str = "3333";

/// Compile-time fallback when `POOL_PROTOCOL` is unset in the
/// environment. The workspace `.cargo/config.toml` sets it to
/// `"sv2"`, which overrides this constant for every normal
/// firmware and host build; this `"sv1"` only takes effect when a
/// crate is built without the workspace config in scope.
pub const DEFAULT_POOL_PROTOCOL: &str = "sv1";

/// Delay between reconnect attempts when the TCP session drops or
/// the stratum loop returns. Same shape as the other defaults but
/// deliberately not env-configurable — adjust the constant if a
/// board needs a different cadence.
pub const RECONNECT_DELAY: Duration = Duration::from_secs(5);

/// TCP socket buffer size for the stratum connection. Smaller than
/// the HTTP/TLS buffers in `consts.rs` because stratum JSON-RPC
/// messages are short.
pub const STRATUM_TCP_BUFFER_SIZE: usize = 2048;

/// TCP socket timeout for the stratum connection. Long enough to
/// survive a few missed pool pings without dropping the session.
pub const SOCKET_TIMEOUT: Duration = Duration::from_secs(120);

/// Maximum byte length of `POOL_HOST` (DNS name or IPv4 literal)
/// and `POOL_USER`. Mining pool hostnames are short in practice;
/// 128 covers the longest reasonable cases.
pub const MAX_HOST_LEN: usize = 128;
pub const MAX_USER_LEN: usize = 128;
pub const MAX_PASS_LEN: usize = 128;

/// Parsed result of the seven `POOL_*` env vars.
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
        /// `None` → `POOL_SV2_TRUST_ANY_KEY=1` was set, so the noise
        /// handshake proceeds without validating the pool's static key
        /// (`ValidateNothing`). `Some` → `ValidateSignature` against
        /// the supplied authority key; the default is the Braiins prod
        /// key from [`DEFAULT_POOL_SV2_PUBKEY`].
        pool_pubkey: Option<[u8; 32]>,
    },
}

/// Failure modes for [`parse`]. Each is a string-shape problem
/// caught at boot; the firmware logs the variant and skips
/// spawning the stratum task so the rest of the board boots
/// normally and the operator can read the error from the defmt
/// stream rather than chasing a reset loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigError {
    HostTooLong,
    UserTooLong,
    PassTooLong,
    InvalidPort,
    InvalidProtocol,
    InvalidPubkey,
    /// `POOL_SV2_TRUST_ANY_KEY` was set to a value other than `"1"`.
    /// Only `"1"` is accepted; any other value (including empty string
    /// and `"true"`) is rejected so a mistyped override fails loudly
    /// rather than silently enabling authentication when the operator
    /// intended to disable it.
    InvalidTrustAnyKey,
}

/// Parse the seven raw env-var inputs into a typed [`PoolConfig`].
///
/// Callers (firmware crates) read each var via `option_env!`. For
/// the four required strings they apply the matching `DEFAULT_*`
/// constant as fall-through and pass a `&str`. For `POOL_SV1_PASS`
/// and `POOL_SV2_TRUST_ANY_KEY` they pass the `Option<&str>` straight
/// through — an unset env var is just `None`, no sentinel string
/// required. `sv2_pubkey_str` is always a `&str`; callers apply
/// `DEFAULT_POOL_SV2_PUBKEY` as the fall-through so the function
/// always has a key to decode when the SV2 path is taken. The
/// function is `no_std`-friendly and runs once at boot.
///
/// # Errors
///
/// See [`ConfigError`] for the exhaustive list. The firmware boot
/// path matches on the result: on `Err` it logs the variant via
/// defmt and skips spawning the stratum task, so a typo in
/// `POOL_SV2_PUBKEY` surfaces as a single error line rather than
/// a watchdog-driven reset loop.
pub fn parse(
    host: &str,
    port_str: &str,
    protocol_str: &str,
    user: &str,
    sv1_pass: Option<&str>,
    sv2_pubkey_str: &str,
    sv2_trust_any_key: Option<&str>,
) -> Result<PoolConfig, ConfigError> {
    let host = String::<MAX_HOST_LEN>::from_str(host).map_err(|()| ConfigError::HostTooLong)?;
    let port: u16 = port_str.parse().map_err(|_| ConfigError::InvalidPort)?;
    let user = String::<MAX_USER_LEN>::from_str(user).map_err(|()| ConfigError::UserTooLong)?;
    let protocol = parse_protocol(protocol_str, sv1_pass, sv2_pubkey_str, sv2_trust_any_key)?;

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
    sv2_pubkey_str: &str,
    sv2_trust_any_key: Option<&str>,
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
            let pool_pubkey = match sv2_trust_any_key {
                Some("1") => None,
                Some(_) => return Err(ConfigError::InvalidTrustAnyKey),
                None => Some(decode_pubkey(sv2_pubkey_str)?),
            };
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
