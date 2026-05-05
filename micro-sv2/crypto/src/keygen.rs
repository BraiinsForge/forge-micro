//! RNG abstraction for the SV2 noise handshake.
//!
//! Implementors plug in the platform's secure random source (OS RNG,
//! hardware RNG on embedded targets, etc.).

use crate::keys::{Secp256k1KeyPair, SecretSecp256k1Key};

pub trait KeyGen {
    /// Produce 32 bytes of cryptographically secure entropy.
    fn new_256_bit_entropy(&mut self) -> [u8; 32];

    /// Sample a fresh secp256k1 private key.
    fn new_private_key(&mut self) -> SecretSecp256k1Key {
        todo!()
    }

    /// Sample a fresh keypair (secret + x-only public + ellswift encoding).
    fn new_keypair<C: secp256k1::Signing + secp256k1::Verification>(
        &mut self,
        _ctx: &secp256k1::Secp256k1<C>,
    ) -> Secp256k1KeyPair {
        todo!()
    }
}
