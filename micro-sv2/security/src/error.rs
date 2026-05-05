//! Alloc-free error types.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthenticationError {
    /// Certificate's `valid_from` is later than `not_valid_after`.
    BrokenCertificate,
    /// `Clock::now_unix_secs` failed or returned an implausible value.
    SystemTimeInvalid,
    /// Now is earlier than `valid_from`.
    CertificateNotYetValid,
    /// Now is later than `not_valid_after`.
    CertificateNoMoreValid,
    /// Signature does not verify against the certificate header.
    SignatureInvalid,
    /// Authority public key is not a valid x-only point.
    BadPublicKey,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyParseError {
    /// Encoded form has the wrong length or layout.
    BadFormat,
    /// Decoded bytes do not match the public-key encoding (wrong version
    /// byte or length).
    NotAPublicKey,
    /// Decoded bytes do not match the secret-key encoding (missing `priv`
    /// prefix or wrong length).
    NotAPrivateKey,
    /// Base58check checksum failed or the input is not valid base58.
    InvalidChecksum,
    /// Underlying secp256k1 byte string is not a valid scalar / point.
    InvalidKey,
}

impl From<ii_sv2_crypto::Secp256k1KeyError> for KeyParseError {
    fn from(_: ii_sv2_crypto::Secp256k1KeyError) -> Self {
        Self::InvalidKey
    }
}
