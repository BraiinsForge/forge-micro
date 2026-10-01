// Copyright (C) 2026  Braiins Systems s.r.o.
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.
//
// Braiins Systems s.r.o. reserves the right to grant any party a license
// to this program, or any part thereof, under any terms, and such a grant
// shall be considered distinct from the grant above.

//! Error type returned by `MiningClient` operations.

/// Failure modes that can surface from a `MiningClient` impl. They are
/// coarse because an adapter wraps its own protocol errors behind
/// `Adapter` rather than leaking them across the trait boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum Error {
    /// Underlying transport (TCP, TLS, Noise) failed.
    Transport,
    /// A queued frame still occupies the outbound buffer. Retry after
    /// `drive` makes progress.
    Busy,
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
    /// The adapter does not implement the operation. V1 carries no
    /// extension frames.
    NotImplemented,
    /// Catch-all for adapter-internal protocol violations.
    Adapter,
}

/// Wire-level cause of a pool-side session rejection, carried inside
/// [`Error::SessionRejected`]. The adapter goes to a terminal rejected
/// state when one arrives, so the next `drive` or `submit` surfaces it.
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
    /// Pool advertised an extranonce longer than
    /// [`MAX_EXTRANONCE_2`](crate::MAX_EXTRANONCE_2), which would
    /// truncate into the per-share buffer and waste every share.
    ExtranonceTooLarge,
    /// Pool granted no version rolling. Without it a chain searches only
    /// the 32-bit nonce of each header, which it exhausts in
    /// milliseconds and then hashes again.
    VersionRollingRefused,
}
