# ii-sv2-crypto

`no_std`, no-alloc Stratum V2 noise core. Implements
`Noise_NX_Secp256k1+EllSwift_ChaChaPoly_SHA256` — the SV2 handshake — without
ring or std/alloc, so it can run on constrained mining devices.

Functionally identical to the noise-core crate in
[`stratum-v2`](https://gitlab.ii.zone/redacted/), with
RustCrypto primitives in place of ring.

## Workflow

1. **Implement [`KeyGen`].** Plug in the platform's CSPRNG and a long-lived
   `secp256k1::Secp256k1` context.
2. **Generate the responder's static keypair** with
   `KeyGen::new_keypair()`. This is what authenticates the responder.
3. **Drive the handshake.** Alternate `write_message` / `read_message`,
   carrying bytes over the wire, until both sides report `finish()`.
4. **Use the [`TransportState`]** for application data: `write_message` /
   `write_message_inplace` to seal, `read_message` / `read_message_inplace`
   to open.

The initiator does not have a static key in NX — only the responder does.

## Quick example

```rust
use ii_sv2_crypto::{HandShake, KeyGen};

struct MyKeyGen { /* ... your RNG + secp256k1 ctx ... */ }
impl KeyGen for MyKeyGen { /* ... */ }

// Responder generates a long-term static keypair.
let static_kp = MyKeyGen::new().new_keypair();

// Both sides set up.
let mut initiator = HandShake::new_initiator(MyKeyGen::new());
let mut responder = HandShake::new_responder(MyKeyGen::new(), static_kp);

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

- `secp256k1` with `default-features = false`. The `Secp256k1::new()`
  constructor requires `alloc`; embedded users build a context with
  `Secp256k1::preallocated_*` over a `static` buffer instead.
- `sha2`, `hkdf`, `chacha20poly1305` — RustCrypto, all `default-features = false`.
- No `String`, no `Vec`, no `Box` anywhere in the public or internal API.

## References

- Stratum V2 spec: https://github.com/stratum-mining/sv2-spec/
- Noise specification: https://noiseprotocol.org/noise.html
- BIP-324 (ellswift / v2 ECDH): https://github.com/bitcoin/bips/blob/master/bip-0324.mediawiki
