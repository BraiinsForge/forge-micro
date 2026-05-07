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
//!
//! ## Reusable crypto context (std)
//!
//! Build the secp256k1 precomputation tables **once** at startup and
//! reuse the same `CryptoCtx` for every handshake.
//!
//! ```rust,ignore
//! use std::sync::OnceLock;
//! use ii_sv2::security::{secp256k1, CryptoCtx, Rng};
//!
//! static CTX: OnceLock<CryptoCtx<secp256k1::All>> = OnceLock::new();
//! fn ctx() -> &'static CryptoCtx<secp256k1::All> { CTX.get_or_init(CryptoCtx::new) }
//!
//! struct MyRng;
//! impl Rng for MyRng {
//!     fn fill_bytes(&mut self, dest: &mut [u8]) {
//!         // Plug in your platform CSPRNG (e.g. `rand::rng().fill_bytes(dest)`).
//!         # let _ = dest;
//!     }
//! }
//!
//! // Use it: `ii_sv2::security::initiate(&mut io, MyRng, ctx(), authenticator).await`
//! ```
//!
//! ## Reusable crypto context (no_std embedded)
//!
//! On embedded targets without an allocator, supply a static buffer
//! sized via [`security::CryptoCtx::preallocated_size`].
//!
//! ```rust,ignore
//! use ii_sv2::security::{secp256k1, AlignedType, CryptoCtx};
//!
//! // Compile-time-known size. AllPreallocated covers signing + verification
//! // (the same shape as All, but with no heap allocation).
//! const SIZE: usize = CryptoCtx::<secp256k1::AllPreallocated<'static>>::preallocated_size();
//! static mut CTX_BUF: [AlignedType; SIZE] = [AlignedType::ZERO; SIZE];
//!
//! // At startup, before any handshake:
//! let ctx = unsafe {
//!     CryptoCtx::preallocated(&mut CTX_BUF).expect("preallocated context init")
//! };
//! // Pass `&ctx` to every `initiate` / `accept` call.
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
