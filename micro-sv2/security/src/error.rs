//! Alloc-free error types.

/// Top-level error returned by the async handshake driver. Generic over
/// the underlying `embedded_io_async` error so callers can keep their
/// concrete IO error in the chain without `Box<dyn Error>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitializationError<E> {
    Io(E),
    /// Peer closed the connection mid-handshake.
    UnexpectedEof,
    Authentication(AuthenticationError),
    Handshake(ii_sv2_crypto::NXError),
    InvalidKey(KeyParseError),
}

impl<E> From<AuthenticationError> for InitializationError<E> {
    fn from(e: AuthenticationError) -> Self {
        Self::Authentication(e)
    }
}

impl<E> From<ii_sv2_crypto::NXError> for InitializationError<E> {
    fn from(e: ii_sv2_crypto::NXError) -> Self {
        Self::Handshake(e)
    }
}

impl<E> From<KeyParseError> for InitializationError<E> {
    fn from(e: KeyParseError) -> Self {
        Self::InvalidKey(e)
    }
}

impl<E> From<embedded_io_async::ReadExactError<E>> for InitializationError<E> {
    fn from(e: embedded_io_async::ReadExactError<E>) -> Self {
        match e {
            embedded_io_async::ReadExactError::UnexpectedEof => Self::UnexpectedEof,
            embedded_io_async::ReadExactError::Other(e) => Self::Io(e),
        }
    }
}

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
