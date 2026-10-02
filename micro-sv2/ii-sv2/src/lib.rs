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

//! Stratum V2 — borrowed-first, `no_std` reference implementation.
//!
//! The umbrella crate re-exports the four implementation crates as
//! modules, so a caller depends on `ii-sv2` alone.
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
//! The secp256k1 precomputation tables are built once at startup and
//! the same `CryptoCtx` serves every handshake.
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
//! A target without an allocator supplies a static buffer sized by
//! [`security::CryptoCtx::preallocated_size`].
//!
//! ```rust,ignore
//! use ii_sv2::security::{secp256k1, AlignedType, CryptoCtx};
//!
//! // AllPreallocated covers signing and verification with no heap.
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
