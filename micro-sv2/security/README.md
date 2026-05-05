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
use ii_sv2_security::{PublicKey, SecretKey};

let ctx = secp256k1::Secp256k1::new(); // or a preallocated context on embedded
let sk = SecretKey::new([0x42; 32]);

// Display gives the base58check string.
let encoded = format!("{}", sk);
assert!(encoded.starts_with("priv"));

// Parse back.
let sk2: SecretKey = encoded.parse().unwrap();
assert_eq!(sk, sk2);

// Derive the matching public key (no global secp256k1 context — pass yours).
let pk = PublicKey::from_secret_key(&sk, &ctx).unwrap();
```

## Quick example — async handshake

```rust
use ii_sv2_security::{accept, initiate, ValidateNothing, EstablishedSession};

// Caller supplies a KeyGen impl (RNG + secp256k1 context). On embedded:
// wrap the platform RNG and a `Secp256k1::preallocated_*` context.
struct MyKeyGen { /* ... */ }
impl ii_sv2_crypto::KeyGen for MyKeyGen { /* ... */ }

// Initiator side: takes any embedded-io-async Read+Write.
async fn client(io: &mut impl embedded_io_async::Read + embedded_io_async::Write) {
    let session = initiate(io, MyKeyGen::new(), ValidateNothing).await.unwrap();
    // Use session.transport for application traffic.
}

// Responder side: needs the certificate bundle (cert + private static key).
async fn server(
    io: &mut impl embedded_io_async::Read + embedded_io_async::Write,
    cert: &ii_sv2_security::CertificateWithPrivateKey,
) {
    let session = accept(io, MyKeyGen::new(), cert).await.unwrap();
    // ...
}
```

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
* `secp256k1` with `default-features = false`. Callers supply their own
  preallocated context.

## References

* SV2 spec: https://github.com/stratum-mining/sv2-spec/
* Underlying noise core: [`ii-sv2-crypto`](../crypto)
