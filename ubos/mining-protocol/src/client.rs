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

//! `MiningClient` and `MiningClientVisitor` — the contract between
//! protocol adapters and the integration layer.
//!
//! Adapters (`stratum-v1::StratumV1Client`, `stratum-v2::StratumV2Client`)
//! implement `MiningClient`. The integration layer
//! (`stratum-client::stratum_task`) drives the adapter and supplies a
//! `MiningClientVisitor` that owns the job ring and work ring.
//!
//! Sync vs async split:
//!
//! - `submit` and `accept` are sync. They encode/queue or
//!   dispatch but do not touch the network.
//! - `connect` and `drive` are async and own the only I/O paths.
//!   `drive` must be cancellation-safe — see the doc comment.

use embedded_io_async::{Read, Write};

use crate::error::Error;
use crate::job::MiningJob;
use crate::session::{SessionUpdate, SubmitResult};
use crate::share::Share;

/// Protocol-side endpoint of a mining session.
///
/// Implementations own the wire buffer, decode scratch (if any), and
/// the protocol state machine. They translate borrowed wire types
/// into the canonical `MiningJob` / `SessionUpdate` / `SubmitResult`
/// shapes for the visitor.
//
// async_fn_in_trait: implementations run on a cooperative
// single-executor; Send bounds on returned futures are neither
// needed nor possible in no_std.
#[allow(async_fn_in_trait)]
pub trait MiningClient {
    /// Kick off protocol-transport setup.
    ///
    /// V1: no-op on the wire — the configure/subscribe/authorize
    /// messages are queued for the next `drive` call.
    ///
    /// V2: performs the Noise NX handshake on `transport`, queues
    /// `SetupConnection` and `OpenStandardMiningChannel` for the next
    /// `drive` call.
    ///
    /// # Errors
    ///
    /// Returns `Err(Error::AuthFailed)` if the pool's certificate is
    /// rejected by the configured authenticator (V2 only), or
    /// `Err(Error::Transport)` on socket error.
    async fn connect<T>(&mut self, transport: &mut T) -> Result<(), Error>
    where
        T: Read + Write;

    /// Encode and queue a share submission. Synchronous: no I/O.
    /// The bytes are flushed by the next `drive` call.
    ///
    /// # Errors
    ///
    /// Returns `Err(Error::UnknownJobId)` if the adapter's
    /// `JobId`-to-wire-id map no longer holds an entry for
    /// `share.id`. Returns `Err(Error::BufferTooSmall)` if the
    /// adapter's outbound buffer is full.
    fn submit(&mut self, share: &Share<'_>) -> Result<(), Error>;

    /// Drive one frame's worth of network I/O.
    ///
    /// Reads bytes into the adapter's wire buffer, decrypts the
    /// header (V2), and stages the frame for `accept` to dispatch.
    /// Also flushes any queued outbound bytes (`submit`'d shares,
    /// handshake messages).
    ///
    /// **Cancellation-safe.** A `drive` future dropped before
    /// completion must not advance crypto state (Noise nonces, AEAD
    /// counters); the next call resumes the read cleanly.
    /// Implementations achieve this by structuring each call as one
    /// atomic header read; the payload AEAD-open happens inside
    /// `accept`, after the full frame has been buffered.
    ///
    /// # Errors
    ///
    /// Returns `Err(Error::Transport)` on socket error,
    /// `Err(Error::Decode)` if a buffered frame fails to parse, or
    /// `Err(Error::SessionRejected(cause))` if the pool tore the
    /// session down with a fatal protocol-layer error. The `cause`
    /// identifies which wire-level message ended the session.
    async fn drive<T>(&mut self, transport: &mut T) -> Result<(), Error>
    where
        T: Read + Write;

    /// Dispatch the most recently buffered frame to `visitor`.
    ///
    /// Synchronous. Decodes the payload (AEAD-open for V2,
    /// hex-decode for V1) into the adapter's decode scratch, builds
    /// the borrowed `MiningJob` / `SessionUpdate` / `SubmitResult`
    /// values, and calls the matching visitor method.
    ///
    /// All borrows handed to the visitor are valid only for the
    /// duration of this call; the visitor must copy out anything it
    /// needs to keep.
    fn accept<V: MiningClientVisitor>(&mut self, visitor: &mut V);
}

/// Receiving side of the trait — owned by the integration layer.
///
/// Methods are sync and `&mut self`. Lifetimes on arguments bind to
/// the `accept` call frame on the `MiningClient` side, so the
/// visitor can borrow but cannot escape data outside the call.
pub trait MiningClientVisitor {
    /// New mining job available.
    fn visit_new_job(&mut self, job: MiningJob<'_>);

    /// Pool reported the outcome of a previously submitted share.
    fn visit_submit_result(&mut self, result: SubmitResult<'_>);

    /// Pool changed a session-level parameter (target, extranonce,
    /// version mask, or reconnect request).
    fn visit_session_update(&mut self, update: SessionUpdate<'_>);
}
