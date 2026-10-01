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

//! Static-dispatch authenticator selector.
//!
//! `Sv2Config::pool_pubkey` decides which built-in `Authenticator`
//! the adapter uses. `ii_sv2::security::initiate` takes a single
//! concrete type, so the two cases are wrapped in one enum that
//! itself implements `Authenticator`.

use ii_sv2::security::{
    AuthenticationError, Authenticator, CertificateHeader, Signature, ValidateNothing,
    ValidateSignature, secp256k1,
};

/// Either no validation (test pools) or signature validation against
/// the configured pool authority public key.
pub enum AuthChoice<'a, C: secp256k1::Verification> {
    Nothing(ValidateNothing),
    Signature(ValidateSignature<'a, C>),
}

impl<C: secp256k1::Verification> Authenticator for AuthChoice<'_, C> {
    fn check(&self, hdr: CertificateHeader, sig: Signature) -> Result<(), AuthenticationError> {
        match self {
            Self::Nothing(v) => v.check(hdr, sig),
            Self::Signature(v) => v.check(hdr, sig),
        }
    }
}
