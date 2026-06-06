//! Pool endpoint configuration and validation.
//!
//! Defines [`PoolEntry`], [`ProtocolKind`], the factory-default pool
//! constants, and per-entry validation.

/// Factory-default pool host for [`crate::Config::default`]. Overridable at
/// compile time by setting the `POOL_HOST` env var; a stored config
/// in flash supersedes both.
pub const DEFAULT_POOL_HOST: &str = "stratum.braiins.com";

/// Factory-default pool port. Overridable via the `POOL_PORT` env var.
pub const DEFAULT_POOL_PORT: u16 = 3333;

/// Factory-default worker name. Empty by default as a sentinel meaning
/// "operator has not configured a worker yet" — most pools accept an
/// empty worker and authorise on a default account, so this fails open;
/// the dashboard should prompt the operator to set a real worker before
/// shipping. Overridable via the `POOL_USER` env var.
pub const DEFAULT_POOL_USER: &str = "";

/// Factory-default SV2 authority public key for `stratum.braiins.com`,
/// pre-decoded from base58check `9awtMD5KQgvRUh2yFbjVeT7b6hjipWcAsQHd6wEhgtDT9soosna`
/// to its 32-byte xonly form. An unconfigured firmware reaches the
/// Braiins prod endpoint with `ValidateSignature` rather than
/// `ValidateNothing`.
pub const DEFAULT_POOL_SV2_PUBKEY: [u8; 32] = [
    0x29, 0x95, 0x4c, 0x97, 0x45, 0x6b, 0x59, 0x9c, 0x83, 0x6f, 0x70, 0xf5, 0x08, 0xd6, 0x19, 0xd3,
    0x28, 0x52, 0x7b, 0xb8, 0x72, 0x29, 0xd7, 0xd7, 0xd2, 0xe6, 0x7e, 0x6d, 0xcd, 0x18, 0xeb, 0xcf,
];

/// Selects the pool mining protocol and carries its protocol-specific
/// parameters.
#[derive(
    Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, postcard_schema::Schema,
)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum ProtocolKind {
    /// Stratum V1 with an optional worker password.
    Sv1 {
        /// `None` means no password sent; most pools authorise by
        /// worker name alone.
        password: Option<heapless::String<128>>,
    },
    /// Stratum V2 with an optional authority public key.
    Sv2 {
        /// `None` means `ValidateNothing`; `Some` means
        /// `ValidateSignature` against the supplied 32-byte key.
        pool_pubkey: Option<[u8; 32]>,
    },
}

/// A single pool endpoint entry.
#[derive(
    Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, postcard_schema::Schema,
)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct PoolEntry {
    /// DNS name or IPv4 literal of the pool host.
    pub host: heapless::String<128>,
    /// TCP port.
    pub port: u16,
    /// Worker name sent in `mining.authorize`.
    pub user: heapless::String<128>,
    /// Protocol selection and its parameters.
    pub protocol: ProtocolKind,
}

/// Maximum number of pool slots the firmware persists and validates.
///
/// Drives both the `heapless::Vec<PoolEntry, MAX_POOLS>` capacity in
/// [`crate::Config`] / [`crate::ConfigUpdate`] and the length of the
/// per-slot error-message tables below, so growing the slot count is
/// a single-const change that fails to compile until every table is
/// extended in step.
pub const MAX_POOLS: usize = 3;

const POOL_HOST_EMPTY: [&str; MAX_POOLS] = [
    "pool 1: host must not be empty",
    "pool 2: host must not be empty",
    "pool 3: host must not be empty",
];

const POOL_PORT_ZERO: [&str; MAX_POOLS] = [
    "pool 1: port must be greater than 0",
    "pool 2: port must be greater than 0",
    "pool 3: port must be greater than 0",
];

impl PoolEntry {
    /// Validate this entry against basic connectivity requirements.
    ///
    /// `slot` is the zero-based index into the pool list; the error
    /// message includes the human-readable slot number so the operator
    /// knows which of the UI rows failed. The method covers the full
    /// [`MAX_POOLS`] capacity; out-of-range slot indices fall back to a
    /// generic message rather than panicking, so the public API stays
    /// total even if a future caller iterates an unbounded source.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the host is empty or the port is zero.
    pub fn validate(&self, slot: usize) -> Result<(), &'static str> {
        if self.host.is_empty() {
            return Err(POOL_HOST_EMPTY
                .get(slot)
                .copied()
                .unwrap_or("pool: host must not be empty"));
        }
        if self.port == 0 {
            return Err(POOL_PORT_ZERO
                .get(slot)
                .copied()
                .unwrap_or("pool: port must be greater than 0"));
        }
        Ok(())
    }
}
