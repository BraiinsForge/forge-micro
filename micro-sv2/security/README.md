# ii-sv2-security

User-friendly Stratum V2 security types built on
[`ii-sv2-crypto`](../crypto). Mirrors the relevant parts of
`redacted` from
[`stratum-v2`](https://gitlab.ii.zone/redacted/) — but
`no_std`, no-alloc, and runtime-agnostic.

## Scope

* SV2-spec key encodings: `SecretKey`, `PublicKey`, `Signature`,
  `SessionId` with base58check `Display` / `FromStr`.
* Certificate types: `Certificate`, `CertificateSigningRequest`,
  `CertificateWithPrivateKey`.
* Authenticators: `Authenticator` trait + `ValidateNothing`,
  `ValidateSignature`, `ValidateTime`, `ValidateFull`. Time-aware
  validators take a user-supplied `Clock` trait — there's no
  `std::time` in `no_std`.
* Async, runtime-agnostic handshake driver: `initiate` and `accept`
  generic over `embedded_io_async::{Read, Write}`. No `embassy-*` or
  `tokio` in this crate's runtime dependencies.

Out of scope: the deprecated noise certificate formats from
`deprecated-noise`, the tokio `Framed` codec, and JSON certificate
serialization.

## Encodings

| Type | Prefix | Encoding |
|---|---|---|
| `SecretKey` | `0x109b3700` (4 bytes, big-endian) | base58check; result always starts with `"priv"` |
| `PublicKey` | `[1, 0]` (2 bytes, little-endian version 1) | base58check |
| `Signature` | none | plain base58 (64 bytes) |
| `SessionId` | none | plain base58 (32 bytes — the noise handshake hash) |

## Quick example — keys

```rust
use core::str::FromStr;
use ii_sv2_security::{CryptoCtx, PublicKey, SecretKey};

let ctx = CryptoCtx::new();
let sk = SecretKey::new([0x42; 32]);

let encoded = format!("{}", sk);
assert!(encoded.starts_with("priv"));

let sk2: SecretKey = encoded.parse().unwrap();
assert_eq!(sk, sk2);

// Derive the matching public key (no global secp256k1 context — pass yours).
let pk = PublicKey::from_secret_key(&sk, ctx.as_secp256k1()).unwrap();
```

## Quick example — async handshake

```rust
use ii_sv2_security::{CryptoCtx, Rng, accept, initiate, ValidateNothing};

// Caller supplies an `Rng` impl. One method.
struct MyRng;
impl Rng for MyRng {
    fn fill_bytes(&mut self, dest: &mut [u8]) {
        // Plug in the platform CSPRNG.
    }
}

// Build the secp256k1 context once and reuse.
async fn client(io: &mut (impl embedded_io_async::Read + embedded_io_async::Write)) {
    let ctx = CryptoCtx::new();
    let session = initiate(io, MyRng, &ctx, ValidateNothing).await.unwrap();
    // Use session.transport for application traffic.
}

async fn server(
    io: &mut (impl embedded_io_async::Read + embedded_io_async::Write),
    cert: &ii_sv2_security::CertificateWithPrivateKey,
) {
    let ctx = CryptoCtx::new();
    let session = accept(io, MyRng, &ctx, cert).await.unwrap();
    // ...
}
```

For an embedded `no_std` target, replace `CryptoCtx::new()` with
`CryptoCtx::preallocated(&mut buf)` over a static buffer. See the
umbrella crate's rustdoc for a worked example.

The handshake driver does no `spawn`, no timers, and no async
synchronisation primitives — it is just a `Future`. Whoever polls it
(embassy executor on a device, tokio in tests, smol, custom) can run
it.

## no_std story

* `bs58` with `default-features = false, features = ["check"]`.
  Encoding writes into a fixed-size stack buffer (`bs58::encode().onto(&mut [u8])`)
  and `Display` impls then `f.write_str` the result — no `String`, no `Vec`.
* No `std::time` — `ValidateTime` (next phase) takes a user-supplied
  `Clock` trait.
* `secp256k1` with `default-features = false`. The default `alloc`
  feature on this crate enables `CryptoCtx::new()`; for embedded
  targets use `CryptoCtx::preallocated` over a static buffer.

## References

* SV2 spec: https://github.com/stratum-mining/sv2-spec/
* Underlying noise core: [`ii-sv2-crypto`](../crypto)
