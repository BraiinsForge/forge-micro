//! `StratumV2Client` — Stratum V2 adapter wrapping `ii-sv2`.
//!
//! Skeleton commit: the struct shape and trait impl land here
//! returning `NotImplemented` for everything; subsequent commits
//! fill in the Noise handshake (`connect`), the encrypted-frame
//! read loop (`drive`), and the `MiningMessages` dispatch
//! (`accept`).

use embedded_io_async::{Read, Write};
use mining_protocol::{Error, MiningClient, MiningClientVisitor, Share};

use crate::config::Sv2Config;

/// Wire buffer sized for the largest standard-channel mining
/// message we expect plus the SV2 noise framing overhead. Tuned
/// alongside the codec impl in the next commits.
pub const WIRE_BUFFER_BYTES: usize = 1024;

/// Outbound buffer for one encoded message. Sized for
/// `OpenStandardMiningChannel` and `SubmitSharesStandard` with
/// noise framing headroom.
pub const ENCODE_BUFFER_BYTES: usize = 512;

/// Stratum V2 protocol adapter. Holds the session config and
/// (eventually) the noise transport state, the encrypted codec,
/// the `JobId` translation map, and the channel id.
pub struct StratumV2Client {
    #[allow(dead_code)]
    config: &'static Sv2Config,
    #[allow(dead_code)]
    wire_buffer: [u8; WIRE_BUFFER_BYTES],
    #[allow(dead_code)]
    encode_buffer: [u8; ENCODE_BUFFER_BYTES],
    // Noise transport, EncryptedCodec, channel_id, JobId map land
    // alongside the connect/drive/accept impls in the next commits.
}

impl StratumV2Client {
    /// Construct an adapter ready for `connect`.
    #[must_use]
    pub const fn new(config: &'static Sv2Config) -> Self {
        Self {
            config,
            wire_buffer: [0; WIRE_BUFFER_BYTES],
            encode_buffer: [0; ENCODE_BUFFER_BYTES],
        }
    }
}

impl MiningClient for StratumV2Client {
    async fn connect<T>(&mut self, _transport: &mut T) -> Result<(), Error>
    where
        T: Read + Write,
    {
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
        // Stub. Dispatch lands once `connect` and `drive` are real.
    }
}
