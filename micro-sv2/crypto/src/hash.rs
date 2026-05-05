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

#[cfg(test)]
mod tests {
    use super::*;

    /// Reference vector from noise-core: `mix_hash([42; 32], [64; 16])` —
    /// only the first four bytes are pinned; the rest follow if the prefix
    /// matches.
    #[test]
    fn mix_hash_first_bytes() {
        let h = [42_u8; 32];
        let data = [64_u8; 16];
        let h = mix_hash(&h, &data);
        assert_eq!(&h[..4], &[0x8e, 0x9c, 0x5b, 0x0e]);
    }

    /// Algebraic check: `hkdf` must equal `(t1, t2)` where
    /// `prk = HMAC(ck, ikm)`, `t1 = HMAC(prk, [1])`, `t2 = HMAC(prk, t1 || [2])`.
    #[test]
    fn hkdf_matches_literal_definition() {
        let ck = [42_u8; 32];
        let ikm = [64_u8; 32];
        let (ref_k1, ref_k2) = reference_hkdf(&ck, &ikm);
        let (k1, k2) = hkdf(&ck, &ikm);
        assert_eq!(k1, ref_k1);
        assert_eq!(k2, ref_k2);
    }

    fn reference_hkdf(ck: &[u8; 32], ikm: &[u8]) -> ([u8; 32], [u8; 32]) {
        let prk = hmac_sha256(ck, ikm);
        let t1 = hmac_sha256(&prk, &[1]);
        let mut input2 = [0_u8; 33];
        input2[..32].copy_from_slice(&t1);
        input2[32] = 2;
        let t2 = hmac_sha256(&prk, &input2);
        (t1, t2)
    }

    fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
        use sha2::{Digest, Sha256};
        let mut k = [0_u8; 64];
        if key.len() > 64 {
            let h = Sha256::digest(key);
            k[..32].copy_from_slice(&h);
        } else {
            k[..key.len()].copy_from_slice(key);
        }
        let mut ipad = [0x36_u8; 64];
        let mut opad = [0x5c_u8; 64];
        for i in 0..64 {
            ipad[i] ^= k[i];
            opad[i] ^= k[i];
        }
        let inner = {
            let mut h = Sha256::new();
            h.update(ipad);
            h.update(msg);
            h.finalize()
        };
        let mut outer = Sha256::new();
        outer.update(opad);
        outer.update(inner);
        outer.finalize().into()
    }
}
