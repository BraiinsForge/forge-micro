//! Cryptographically-secure entropy source.
//!
//! Implementors plug in the platform's secure random source (OS RNG,
//! hardware RNG on embedded targets, etc.). The trait deliberately
//! exposes only raw bytes — no curve-specific knowledge required.

pub trait Rng {
    /// Fill `dest` with cryptographically secure random bytes.
    ///
    /// The contract is **infallible**: implementors should panic or
    /// block on entropy starvation rather than return short. Embedded
    /// implementations that wrap a hardware RNG should spin until the
    /// peripheral has enough entropy.
    fn fill_bytes(&mut self, dest: &mut [u8]);

    /// Convenience: produce 32 bytes of entropy.
    fn next_256_bits(&mut self) -> [u8; 32] {
        let mut out = [0u8; 32];
        self.fill_bytes(&mut out);
        out
    }
}
