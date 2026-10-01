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
//! (`stratum-client::run_stratum_client`) drives the adapter and
//! supplies a `MiningClientVisitor` that routes mining events to the
//! state owning the job ring and work ring, and frames of another
//! extension to the session extension.
//!
//! Sync vs async split:
//!
//! - `submit`, `send_extension` and `accept` are sync. They
//!   encode/queue or dispatch but do not touch the network.
//! - `connect` and `drive` are async and own the only I/O paths.
//!   `drive` must be cancellation-safe — see the doc comment.

use embedded_io_async::{Read, Write};

use crate::error::Error;
use crate::extension::ExtensionFrame;
use crate::job::MiningJob;
use crate::session::{SessionUpdate, SubmitResult};
use crate::share::Share;

/// Progress made by one call to [`MiningClient::drive`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriveEvent {
    /// A received frame is ready for [`MiningClient::accept`].
    FrameReceived,
    /// The outbound slot is free and the receive path needs more bytes.
    OutboundFlushed,
}

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
    /// V1: sends the configure, subscribe and authorize requests.
    ///
    /// V2: performs the Noise NX handshake on `transport`, then sends
    /// `SetupConnection` and `OpenStandardMiningChannel`. Both adapters
    /// receive their responses through `drive` and `accept`.
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
    /// `share.id`. Returns `Err(Error::Busy)` while the outbound buffer
    /// holds a queued frame, or `Err(Error::BufferTooSmall)` when the
    /// encoded share exceeds its capacity.
    fn submit(&mut self, share: &Share<'_>) -> Result<(), Error>;

    /// The user the session authorises as, which the pool knows the
    /// session by.
    fn user(&self) -> &str;

    /// Encode and queue one frame of another extension. Synchronous:
    /// no I/O. The bytes are flushed by the next `drive` call, after
    /// any share queued before them. An adapter whose protocol carries
    /// no extensions keeps this default.
    ///
    /// # Errors
    ///
    /// Returns `Err(Error::NotImplemented)` from the default body.
    /// Returns `Err(Error::Busy)` while a queued frame waits to be
    /// flushed, `Err(Error::BufferTooSmall)` when the payload exceeds
    /// the outbound capacity, and `Err(Error::Adapter)` before
    /// `connect` completes.
    fn send_extension(&mut self, frame: &ExtensionFrame<'_>) -> Result<(), Error> {
        let _ = frame;
        Err(Error::NotImplemented)
    }

    /// Flush queued bytes and receive a frame, reporting the progress.
    ///
    /// Reads bytes into the adapter's wire buffer, decrypts the
    /// frame (V2), and stages it for `accept` to dispatch. Returns
    /// `FrameReceived` when a frame is ready. A completed outbound
    /// write returns `OutboundFlushed` when the receive path would
    /// otherwise wait, so the caller can queue more data at once.
    /// Ready inbound frames take priority over that notification.
    ///
    /// **Cancellation-safe.** A `drive` future dropped before
    /// completion preserves read and write cursors and the crypto
    /// state reached at those cursors. The next call resumes there.
    ///
    /// # Errors
    ///
    /// Returns `Err(Error::Transport)` on socket error,
    /// `Err(Error::Decode)` if a buffered frame fails to parse, or
    /// `Err(Error::SessionRejected(cause))` if the pool tore the
    /// session down with a fatal protocol-layer error. The `cause`
    /// identifies which wire-level message ended the session.
    async fn drive<T>(&mut self, transport: &mut T) -> Result<DriveEvent, Error>
    where
        T: Read + Write;

    /// Dispatch the most recently buffered frame to `visitor`.
    ///
    /// Synchronous. Parses the frame reported by `FrameReceived`,
    /// decodes protocol fields into the adapter's scratch buffer,
    /// builds borrowed values and calls the matching visitor methods.
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
    fn visit_new_job(&mut self, job: MiningJob<'_>);

    /// Pool reported the outcome of a previously submitted share.
    fn visit_submit_result(&mut self, result: SubmitResult<'_>);

    /// Pool changed a session-level parameter (target, extranonce,
    /// version mask, or reconnect request).
    fn visit_session_update(&mut self, update: SessionUpdate<'_>);

    /// A frame of an extension other than mining arrived. A visitor
    /// that carries no extension keeps this default and the frame is
    /// dropped.
    fn visit_extension_frame(&mut self, frame: ExtensionFrame<'_>) {
        let _ = frame;
    }
}
