//! RNG abstraction for the SV2 noise handshake.
//!
//! Implementors plug in the platform's secure random source (OS RNG,
//! hardware RNG on embedded targets, etc.) and provide the secp256k1
//! context the rest of the crate uses.

use crate::keys::{Secp256k1KeyPair, SecretSecp256k1Key};

pub trait KeyGen {
    /// secp256k1 context flavor (e.g. [`secp256k1::All`],
    /// [`secp256k1::AllPreallocated`]).
    type Ctx: secp256k1::Signing;

    /// Produce 32 bytes of cryptographically secure entropy.
    fn new_256_bit_entropy(&mut self) -> [u8; 32];

    /// Borrow the secp256k1 context this generator is bound to.
    fn ctx(&self) -> &secp256k1::Secp256k1<Self::Ctx>;

    /// Sample a fresh secp256k1 private key.
    fn new_private_key(&mut self) -> SecretSecp256k1Key {
        todo!()
    }

    /// Sample a fresh keypair (secret + x-only public + ellswift encoding).
    fn new_keypair(&mut self) -> Secp256k1KeyPair {
        todo!()
    }
}
