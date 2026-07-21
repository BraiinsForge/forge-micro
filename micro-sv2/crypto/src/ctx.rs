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

//! Long-lived secp256k1 precomputation context.
//!
//! `CryptoCtx` wraps `secp256k1::Secp256k1<C>`. The context is built
//! once at startup and reused for every handshake / keypair operation;
//! it holds no secret state — only precomputed multiplication tables.
//!
//! Two constructors, picked by target:
//!
//! - [`CryptoCtx::new`] (requires the `alloc` feature) — heap-allocates
//!   the precomputation tables. Suitable for std or alloc-enabled
//!   targets. Only available with `feature = "alloc"`.
//! - [`CryptoCtx::preallocated`] — uses a caller-supplied buffer.
//!   Suitable for `no_std` embedded targets that cannot allocate.

use secp256k1::{Secp256k1, ffi::types::AlignedType};

/// Wrapper around `secp256k1::Secp256k1<C>` exposed as the crypto
/// context that every public crypto operation in this workspace needs.
pub struct CryptoCtx<C: secp256k1::Signing + secp256k1::Verification> {
    inner: Secp256k1<C>,
}

// `secp256k1::All` is itself gated on `secp256k1/alloc`, so the
// alloc-only constructors are only meaningful when alloc is on.
#[cfg(feature = "alloc")]
impl CryptoCtx<secp256k1::All> {
    /// Build a context using heap-allocated precomputation tables.
    /// Requires the `alloc` feature (on by default).
    pub fn new() -> Self {
        Self {
            inner: Secp256k1::new(),
        }
    }
}

#[cfg(feature = "alloc")]
impl Default for CryptoCtx<secp256k1::All> {
    fn default() -> Self {
        Self::new()
    }
}

impl<'buf> CryptoCtx<secp256k1::AllPreallocated<'buf>> {
    /// Build a context using a caller-supplied preallocated buffer.
    /// `buf` must be at least [`Self::preallocated_size`] slots long.
    pub fn preallocated(buf: &'buf mut [AlignedType]) -> Result<Self, secp256k1::Error> {
        Ok(Self {
            inner: Secp256k1::preallocated_new(buf)?,
        })
    }

    /// Number of [`AlignedType`] slots (not bytes) the buffer passed
    /// to [`Self::preallocated`] must hold. Useful for sizing
    /// `static mut BUF: [AlignedType; N]`.
    pub fn preallocated_size() -> usize {
        Secp256k1::<secp256k1::AllPreallocated>::preallocate_size()
    }
}

impl<C: secp256k1::Signing + secp256k1::Verification> CryptoCtx<C> {
    /// Borrow the underlying `secp256k1::Secp256k1<C>`.
    ///
    /// Most callers won't need this — it exists so adjacent crates
    /// (e.g. `ii-sv2-security`) can call into the `secp256k1` API
    /// directly when they need signature verification or certificate
    /// signing without each having to construct their own context.
    pub fn as_secp256k1(&self) -> &secp256k1::Secp256k1<C> {
        &self.inner
    }
}
