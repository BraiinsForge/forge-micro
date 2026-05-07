//! Error type returned by `MiningClient` operations.

/// Failure modes that can surface from a `MiningClient` impl.
///
/// Variants are intentionally coarse — adapters wrap their own
/// protocol-specific errors behind `Adapter` rather than leaking them
/// across the trait boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Underlying transport (TCP, TLS, Noise) failed.
    Transport,
    /// Encoding failed because the supplied buffer is too small.
    BufferTooSmall,
    /// Wire data could not be parsed into a known message.
    Decode,
    /// Pool-side handshake or session setup was rejected.
    SessionRejected,
    /// Authenticator declined the pool's certificate (V2).
    AuthFailed,
    /// `submit` referenced a `JobId` the adapter no longer tracks.
    UnknownJobId,
    /// Operation is not yet implemented (V2 share submission today).
    NotImplemented,
    /// Catch-all for adapter-internal protocol violations.
    Adapter,
}
