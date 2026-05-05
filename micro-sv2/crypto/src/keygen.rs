//! RNG abstraction for the SV2 noise handshake.
//!
//! Implementors plug in the platform's secure random source (OS RNG,
//! hardware RNG on embedded targets, etc.) and provide the secp256k1
//! context the rest of the crate uses.

use crate::keys::{Secp256k1KeyPair, SecretSecp256k1Key};

pub trait KeyGen {
    /// secp256k1 context flavor (e.g. [`secp256k1::All`],
    /// [`secp256k1::AllPreallocated`]). Both capabilities are needed:
    /// `Signing` for x-only public derivation, `Verification` for
    /// elligator encoding.
    type Ctx: secp256k1::Signing + secp256k1::Verification;

    /// Produce 32 bytes of cryptographically secure entropy.
    fn new_256_bit_entropy(&mut self) -> [u8; 32];

    /// Borrow the secp256k1 context this generator is bound to.
    fn ctx(&self) -> &secp256k1::Secp256k1<Self::Ctx>;

    /// Sample a fresh secp256k1 private key.
    fn new_private_key(&mut self) -> SecretSecp256k1Key {
        for _ in 0..10 {
            let bytes = self.new_256_bit_entropy();
            if let Ok(sk) = secp256k1::SecretKey::from_byte_array(bytes) {
                return SecretSecp256k1Key(sk);
            }
        }
        panic!("BUG: failed to generate secp256k1 private key after 10 tries");
    }

    /// Sample a fresh keypair (secret + x-only public + ellswift encoding).
    fn new_keypair(&mut self) -> Secp256k1KeyPair {
        let sk = self.new_private_key();
        let entropy = self.new_256_bit_entropy();
        let ctx = self.ctx();
        let (pk, _) = sk.0.x_only_public_key(ctx);
        let elligator = secp256k1::ellswift::ElligatorSwift::from_seckey(ctx, sk.0, Some(entropy));
        Secp256k1KeyPair {
            sk: sk.0,
            pk,
            elligator,
        }
    }
}
