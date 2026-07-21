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

//! Alloc-free error types for the SV2 noise crypto core.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Secp256k1KeyError {
    InvalidSecretKey,
    InvalidPublicKey,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AeadError(pub &'static str);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NXError {
    /// State machine reached an unexpected state.
    InvalidState,
    /// Input buffer is shorter than the expected handshake message.
    NotEnoughBytes,
    /// Output buffer is too small to hold the decrypted payload.
    TooSmallBuffer,
    /// `finish()` was called after a failed handshake.
    FailedHandshake,
    /// `finish()` was called before the handshake was complete.
    UnfinishedHandshake,
    Aead(AeadError),
    Key(Secp256k1KeyError),
    Secp256k1,
}

impl From<AeadError> for NXError {
    fn from(e: AeadError) -> Self {
        Self::Aead(e)
    }
}

impl From<Secp256k1KeyError> for NXError {
    fn from(e: Secp256k1KeyError) -> Self {
        Self::Key(e)
    }
}

impl From<secp256k1::Error> for NXError {
    fn from(_: secp256k1::Error) -> Self {
        Self::Secp256k1
    }
}
