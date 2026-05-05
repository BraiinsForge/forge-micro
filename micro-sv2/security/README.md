# ii-sv2-security

User-friendly Stratum V2 security types built on
[`ii-sv2-crypto`](../crypto). Mirrors the relevant parts of
`redacted` from
[`stratum-v2`](https://gitlab.ii.zone/redacted/) — but
`no_std`, no-alloc, and runtime-agnostic.

## Scope

* SV2-spec key encodings (this phase): `SecretKey`, `PublicKey`,
  `Signature`, `SessionId` with base58check `Display` / `FromStr`.
* Certificate types and authenticators (next phase): `Certificate`,
  `CertificateSigningRequest`, `CertificateWithPrivateKey`,
  `Authenticator` trait + `ValidateNothing`/`ValidateSignature`/
  `ValidateTime`/`ValidateFull`.
* Async, runtime-agnostic handshake driver (next phase): generic over
  `embedded_io_async::{Read, Write}`, no `embassy-*` or `tokio` in this
  crate's dependency tree.

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

## Quick example

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
