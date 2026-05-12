//! Error type returned by `MiningClient` operations.

/// Failure modes that can surface from a `MiningClient` impl.
///
/// Variants are intentionally coarse — adapters wrap their own
/// protocol-specific errors behind `Adapter` rather than leaking them
/// across the trait boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum Error {
    /// Underlying transport (TCP, TLS, Noise) failed.
    Transport,
    /// Encoding failed because the supplied buffer is too small.
    BufferTooSmall,
    /// Wire data could not be parsed into a known message.
    Decode,
    /// Pool refused this session at the protocol layer. The cause
    /// identifies which wire-level message ended the session so the
    /// caller can decide on backoff or human-readable reporting.
    SessionRejected(SessionRejection),
    /// Authenticator declined the pool's certificate (V2).
    AuthFailed,
    /// `submit` referenced a `JobId` the adapter no longer tracks.
    UnknownJobId,
    /// Operation is not yet implemented (V2 share submission today).
    NotImplemented,
    /// Catch-all for adapter-internal protocol violations.
    Adapter,
}

/// Wire-level cause of a pool-side session rejection.
///
/// Carried inside [`Error::SessionRejected`]. Each variant names the
/// exact V1 or V2 wire message that ended the session; the adapter
/// transitions to a terminal rejected state when one of these
/// arrives, so the next `drive` / `submit` surfaces the cause
/// through this enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum SessionRejection {
    /// V1 `mining.subscribe` returned an error or no result.
    SubscribeRefused,
    /// V1 `mining.authorize` returned an error or boolean `false`.
    AuthorizeRefused,
    /// V2 `SetupConnection.Error` received.
    SetupConnectionError,
    /// V2 `OpenMiningChannel.Error` received.
    OpenMiningChannelError,
    /// Pool advertised an `extranonce_2_size` (V1) or
    /// `extranonce_prefix` length (V2) that exceeds our static
    /// per-share buffer ([`MAX_EXTRANONCE_2`](crate::MAX_EXTRANONCE_2)).
    /// Continuing the session would silently truncate `extranonce_2`
    /// into a too-small buffer and waste every share, so the adapter
    /// refuses the session instead.
    ExtranonceTooLarge,
}
