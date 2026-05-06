//! Stratum V2 — borrowed-first, `no_std` reference implementation.
//!
//! This is the umbrella crate. It re-exports the four implementation
//! crates as modules so callers depend on `ii-sv2` once and write:
//!
//! - `use ii_sv2::types::Str0_255;`
//! - `use ii_sv2::framing::{FrameCodec, Header, PlaintextCodec};`
//! - `use ii_sv2::messages::{SetupConnection, MiningMessages};`
//! - `use ii_sv2::security::{EncryptedCodec, EstablishedSession, accept, initiate};`
//!
//! ## Example: encode and decode a frame in plaintext
//!
//! ```rust
//! use ii_sv2::framing::{FrameCodec, PlaintextCodec, frame_to_message};
//! use ii_sv2::messages::SetupConnectionSuccess;
//!
//! let original = SetupConnectionSuccess { used_version: 2, flags: 0 };
//! let mut codec = PlaintextCodec;
//!
//! // Encode
//! let mut wire = [0u8; 32];
//! let n = codec.encode(&original, &mut wire).unwrap();
//!
//! // Decode header phase
//! let mut hbuf = [0u8; PlaintextCodec::HEADER_WIRE_SIZE];
//! hbuf.copy_from_slice(&wire[..PlaintextCodec::HEADER_WIRE_SIZE]);
//! let header = codec.decode_header(&mut hbuf).unwrap();
//!
//! // Decode payload phase
//! let pn = PlaintextCodec::payload_wire_size(header.msg_length as usize);
//! let mut pbuf = [0u8; 32];
//! pbuf[..pn].copy_from_slice(
//!     &wire[PlaintextCodec::HEADER_WIRE_SIZE..PlaintextCodec::HEADER_WIRE_SIZE + pn],
//! );
//! let frame = codec.decode_frame(header, &mut pbuf[..pn]).unwrap();
//! let received: SetupConnectionSuccess = frame_to_message(frame).unwrap();
//!
//! assert_eq!(received, original);
//! # let _ = n;
//! ```

#![no_std]
#![forbid(unsafe_code)]

pub mod types {
    pub use ii_sv2_types::*;
}

pub mod framing {
    pub use ii_sv2_framing::*;
}

pub mod messages {
    pub use ii_sv2_messages::*;
}

pub mod security {
    pub use ii_sv2_security::*;
}
