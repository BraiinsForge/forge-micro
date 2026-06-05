//! Pool endpoint configuration.
//!
//! Defines [`PoolEntry`], [`ProtocolKind`], and the factory-default
//! pool constants.

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
