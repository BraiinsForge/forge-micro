//! SHA-256 and HKDF helpers used by the noise handshake.

/// Noise `MixHash(h, data) = SHA256(h || data)`.
pub(crate) fn mix_hash(_h: &[u8; 32], _data: &[u8]) -> [u8; 32] {
    todo!()
}

/// Noise `HKDF(chaining_key, ikm) -> (ck', k)` with SHA-256, 64 bytes of OKM.
pub(crate) fn hkdf(
    _chaining_key: &[u8; 32],
    _input_key_material: &[u8],
) -> ([u8; 32], [u8; 32]) {
    todo!()
}
