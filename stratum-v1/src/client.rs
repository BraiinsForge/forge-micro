//! `StratumV1Client` — implements `mining_protocol::MiningClient`
//! by wrapping the wire layer in `crate::wire`.
//!
//! See `boards/eth-examples/IMPLEMENTATION_PLAN.md` and
//! `boards/eth-examples/ARCHITECTURE.md` for the surrounding design.

use embedded_io_async::{Read, Write};

use mining_protocol::{Error, MiningClient, MiningClientVisitor, Share};

use crate::config::Sv1Config;
use crate::scratch::DecodeScratch;
use crate::state::{SessionState, AUTHORIZE_ID, CONFIGURE_ID, SUBSCRIBE_ID};
use crate::wire::line_buffer::LineBuffer;
use crate::wire::messages::{
    encode_authorize, encode_configure, encode_subscribe, Authorize, Configure, Subscribe,
};

#[cfg(test)]
#[path = "tests/client.rs"]
mod tests;

/// Bytes reserved for incoming wire frames (one JSON-RPC line).
/// Matches the value `ubos-backend` settled on; public pools push
/// notifies with merkle paths that fit comfortably here.
pub const WIRE_BUFFER_BYTES: usize = 4096;

/// Bytes reserved for one outgoing message before it hits the
/// transport's buffer. Sized for the largest expected request
/// (`mining.submit`) with headroom.
pub const ENCODE_BUFFER_BYTES: usize = 512;

/// Version-rolling mask requested in `mining.configure`. `1fffe000`
/// is the standard mask Bitmain miners use; `min-bit-count: 2`
/// signals the pool we accept any pool-side reduction down to two
/// bits. Pools that don't support the extension simply omit
/// `version-rolling.mask` from their reply, leaving the negotiated
/// mask at zero.
const CONFIGURE_MAP: &[u8] = br#"{"version-rolling":{"mask":"1fffe000","min-bit-count":2}}"#;

/// Stratum V1 protocol adapter.
///
/// Owns the wire buffer, decode scratch, and persistent session
/// state. Constructed once per session; the firmware drops it and
/// builds a fresh one on reconnect.
pub struct StratumV1Client {
    config: &'static Sv1Config,
    wire_buffer: LineBuffer<WIRE_BUFFER_BYTES>,
    decode_scratch: DecodeScratch,
    session: SessionState,
    encode_buffer: [u8; ENCODE_BUFFER_BYTES],
    /// Bytes pending flush in `encode_buffer`. `0` when nothing is
    /// queued.
    outbound_len: usize,
    /// Cursor into `encode_buffer[..outbound_len]` for resuming a
    /// partially-written flush after a `drive()` cancellation.
    outbound_offset: usize,
}

impl StratumV1Client {
    /// Construct an adapter ready for `connect`.
    #[must_use]
    pub const fn new(config: &'static Sv1Config) -> Self {
        Self {
            config,
            wire_buffer: LineBuffer::new(),
            decode_scratch: DecodeScratch::new(),
            session: SessionState::new(),
            encode_buffer: [0; ENCODE_BUFFER_BYTES],
            outbound_len: 0,
            outbound_offset: 0,
        }
    }

    /// Flush any queued outbound bytes to `transport`.
    ///
    /// Resumable: tracks `outbound_offset` so a cancelled `drive`
    /// future doesn't re-send already-written bytes when `drive` is
    /// next called.
    async fn flush_outbound<T: Write>(&mut self, transport: &mut T) -> Result<(), Error> {
        while self.outbound_offset < self.outbound_len {
            let written = transport
                .write(&self.encode_buffer[self.outbound_offset..self.outbound_len])
                .await
                .map_err(|_| Error::Transport)?;
            if written == 0 {
                return Err(Error::Transport);
            }
            self.outbound_offset += written;
        }
        self.outbound_len = 0;
        self.outbound_offset = 0;
        Ok(())
    }

    /// Send one message synchronously: encode into `encode_buffer`,
    /// write to `transport`. Used during the handshake where we
    /// pipeline configure/subscribe/authorize before returning from
    /// `connect`. After connect, queued outbound goes through
    /// `outbound_len` and `flush_outbound`.
    async fn send_now<T: Write, F>(&mut self, transport: &mut T, encode: F) -> Result<(), Error>
    where
        F: FnOnce(&mut [u8]) -> Result<usize, crate::error::EncodeError>,
    {
        let length = encode(&mut self.encode_buffer).map_err(|_| Error::BufferTooSmall)?;
        let mut offset = 0;
        while offset < length {
            let written = transport
                .write(&self.encode_buffer[offset..length])
                .await
                .map_err(|_| Error::Transport)?;
            if written == 0 {
                return Err(Error::Transport);
            }
            offset += written;
        }
        Ok(())
    }
}

impl MiningClient for StratumV1Client {
    /// V1 has no transport-level handshake (no Noise, no certificate
    /// validation). `connect` pipelines the three handshake
    /// requests — `mining.configure`, `mining.subscribe`,
    /// `mining.authorize` — and returns. Their responses arrive over
    /// the regular `drive`/`accept` loop, where the visitor sees
    /// them as `SessionUpdate` events.
    async fn connect<T>(&mut self, transport: &mut T) -> Result<(), Error>
    where
        T: Read + Write,
    {
        // mining.configure with version-rolling.
        self.send_now(transport, |buf| {
            let configure = Configure {
                features: &["version-rolling"],
                configure_map: CONFIGURE_MAP,
            };
            encode_configure(buf, CONFIGURE_ID, &configure)
        })
        .await?;

        // mining.subscribe — agent string is the user identity, which
        // matches what bosminer and redacted do.
        let agent = self.config.user;
        self.send_now(transport, |buf| {
            let subscribe = Subscribe {
                agent_signature: Some(agent),
                extra_nonce_1: None,
            };
            encode_subscribe(buf, SUBSCRIBE_ID, &subscribe)
        })
        .await?;

        // mining.authorize.
        let user = self.config.user;
        let password = self.config.password;
        self.send_now(transport, |buf| {
            let authorize = Authorize {
                name: user,
                password,
            };
            encode_authorize(buf, AUTHORIZE_ID, &authorize)
        })
        .await?;

        Ok(())
    }

    fn submit(&mut self, _share: &Share<'_>) -> Result<(), Error> {
        // Lands in the next commit alongside the accept dispatch.
        Err(Error::NotImplemented)
    }

    /// One drive call: flush any queued outbound bytes, then read
    /// from `transport` until at least one complete line is buffered
    /// in `wire_buffer`.
    ///
    /// Cancellation safety: `LineBuffer` accumulates incrementally,
    /// `outbound_offset` tracks partial writes, so a cancelled
    /// future leaves the adapter ready to resume.
    async fn drive<T>(&mut self, transport: &mut T) -> Result<(), Error>
    where
        T: Read + Write,
    {
        self.flush_outbound(transport).await?;

        loop {
            // Borrow `wire_buffer` only for the inspection; release
            // before the next read so the mutable borrow on
            // `writable()` is valid.
            let line_present = self
                .wire_buffer
                .next_line()
                .map_err(|_| Error::Decode)?
                .is_some();
            if line_present {
                return Ok(());
            }

            let writable = self.wire_buffer.writable();
            if writable.is_empty() {
                // Line longer than `WIRE_BUFFER_BYTES` — pool is
                // misbehaving. Surface as decode error so the outer
                // loop reconnects.
                return Err(Error::Decode);
            }
            let read_count = transport.read(writable).await.map_err(|_| Error::Transport)?;
            if read_count == 0 {
                return Err(Error::Transport);
            }
            self.wire_buffer.advance(read_count);
        }
    }

    fn accept<V: MiningClientVisitor>(&mut self, _visitor: &mut V) {
        // Drain the pending line so the buffer doesn't fill up.
        // Full dispatch (translation to `MiningJob` /
        // `SessionUpdate` / `SubmitResult`) lands in the next
        // commit; this stub keeps `drive` and `accept` paired so
        // the wire buffer rotates correctly while the dispatch is
        // being filled in.
        self.wire_buffer.consume();
    }
}

// Suppress the dead-code warning on the fields the trait body does
// not yet exercise. The full dispatch in the next commit removes
// the need for this shim.
#[allow(dead_code)]
fn _force_field_use(client: &StratumV1Client) {
    let _ = &client.decode_scratch;
    let _ = &client.session;
}
