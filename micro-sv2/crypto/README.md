# ii-sv2-crypto

`no_std`, no-alloc Stratum V2 noise core. Implements
`Noise_NX_Secp256k1+EllSwift_ChaChaPoly_SHA256` — the SV2 handshake — without
ring or std/alloc, so it can run on constrained mining devices.

Functionally identical to the noise-core crate in
[`stratum-v2`](https://gitlab.ii.zone/redacted/), with
RustCrypto primitives in place of ring.

## Workflow

1. **Implement [`Rng`].** Plug in the platform's CSPRNG. The trait
   has one method, `fill_bytes`.
2. **Build a [`CryptoCtx`] once at startup.** This holds the
   secp256k1 precomputation tables. Reuse it for every operation —
   it has no secret state.
3. **Generate the responder's static keypair** with
   `keys::new_keypair(&mut rng, &ctx)`. This is what authenticates
   the responder.
4. **Drive the handshake.** Alternate `write_message` / `read_message`,
   carrying bytes over the wire, until both sides report `finish()`.
5. **Use the [`TransportState`]** for application data: `write_message` /
   `write_message_inplace` to seal, `read_message` / `read_message_inplace`
   to open.

The initiator does not have a static key in NX — only the responder does.

## Quick example

```rust
use ii_sv2_crypto::{CryptoCtx, HandShake, Rng, keys::new_keypair};

struct MyRng;
impl Rng for MyRng {
    fn fill_bytes(&mut self, dest: &mut [u8]) { /* ... your CSPRNG ... */ }
}

let ctx = CryptoCtx::new();                     // once at startup
let static_kp = new_keypair(&mut MyRng, &ctx);  // responder identity

let mut initiator = HandShake::new_initiator(MyRng, &ctx);
let mut responder = HandShake::new_responder(MyRng, &ctx, static_kp);

let mut wire = [0u8; 256];
let mut scratch = [0u8; 256];

// i -> r:  -> e
let m1 = initiator.write_message(b"", &mut wire)?;
responder.read_message(&wire[..m1], &mut scratch)?;

// r -> i:  <- e, ee, s, es, [cert]
let cert = b"...";
let m3 = responder.write_message(cert, &mut wire)?;
initiator.read_message(&wire[..m3], &mut scratch)?;

// Done — extract transport.
let mut i_transport = initiator.finish()?;
let mut r_transport = responder.finish()?;

// Application data, encrypted in place.
let mut buf = [0u8; 1024];
buf[..msg.len()].copy_from_slice(msg);
let n = i_transport.write_message_inplace(&mut buf, msg.len())?;
r_transport.read_message_inplace(&mut buf[..n])?;
```

For a full annotated walkthrough see [`tests/handshake.rs`](tests/handshake.rs).

## Buffer sizing

Pre-compute these from the wire shape:

| Constant | Value |
|---|---|
| `EPHEMERAL_KEY_MSG_SIZE` | 64 (ellswift-encoded) |
| `STATIC_KEY_MSG_SIZE` | 64 + 16 (ellswift + AEAD tag) |
| `MAC_SIZE` | 16 |

The largest handshake message is the responder's reply:
`EPHEMERAL_KEY_MSG_SIZE + STATIC_KEY_MSG_SIZE + cert_len + MAC_SIZE`.

In-place AEAD decryption needs the receive buffer to hold `ciphertext + MAC`,
not just the plaintext — size accordingly.

## no_std story

- The default `alloc` feature enables `CryptoCtx::new()` (heap-allocated
  context). For `no_std`/no-alloc targets, build with
  `default-features = false` and construct the context with
  `CryptoCtx::preallocated(&mut buf)` over a static buffer instead —
  see the umbrella crate's rustdoc for a worked example.
- `sha2`, `hkdf`, `chacha20poly1305` — RustCrypto, all `default-features = false`.
- No `String`, no `Vec`, no `Box` anywhere in the public or internal API.

## References

- Stratum V2 spec: https://github.com/stratum-mining/sv2-spec/
- Noise specification: https://noiseprotocol.org/noise.html
- BIP-324 (ellswift / v2 ECDH): https://github.com/bitcoin/bips/blob/master/bip-0324.mediawiki
