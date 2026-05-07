//! `StratumV1Client` — implements `mining_protocol::MiningClient`
//! by wrapping the wire layer in `crate::wire`.
//!
//! See `boards/eth-examples/IMPLEMENTATION_PLAN.md` and
//! `boards/eth-examples/ARCHITECTURE.md` for the surrounding design.

use embedded_io_async::{Read, Write};

use mining_protocol::{Error, MiningClient, MiningClientVisitor, Share};

use crate::config::Sv1Config;
use crate::scratch::DecodeScratch;
use crate::state::SessionState;
use crate::wire::line_buffer::LineBuffer;

/// Bytes reserved for incoming wire frames (one JSON-RPC line).
/// Matches the value `ubos-backend` settled on; public pools push
/// notifies with merkle paths that fit comfortably here.
pub const WIRE_BUFFER_BYTES: usize = 4096;

/// Bytes reserved for one outgoing message before it hits the
/// transport's buffer. Sized for the largest expected request
/// (`mining.submit`) with headroom.
pub const ENCODE_BUFFER_BYTES: usize = 512;

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
        }
    }
}

impl MiningClient for StratumV1Client {
    async fn connect<T>(&mut self, _transport: &mut T) -> Result<(), Error>
    where
        T: Read + Write,
    {
        // V1 connect lands in a follow-up: queues the configure /
        // subscribe / authorize messages so the next drive() flushes
        // them. No on-wire setup of its own.
        Err(Error::NotImplemented)
    }

    fn submit(&mut self, _share: &Share<'_>) -> Result<(), Error> {
        Err(Error::NotImplemented)
    }

    async fn drive<T>(&mut self, _transport: &mut T) -> Result<(), Error>
    where
        T: Read + Write,
    {
        Err(Error::NotImplemented)
    }

    fn accept<V: MiningClientVisitor>(&mut self, _visitor: &mut V) {
        // Stub. Full dispatch lands in a follow-up commit.
    }
}

// Suppress the dead-code warning on these fields until the body of
// the trait impl is filled in. The struct shape is the
// load-bearing artefact of this commit.
#[allow(dead_code)]
fn _force_field_use(client: &StratumV1Client) {
    let _ = client.config;
    let _ = &client.wire_buffer;
    let _ = &client.decode_scratch;
    let _ = &client.session;
    let _ = &client.encode_buffer;
}
