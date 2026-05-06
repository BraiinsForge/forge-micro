# TECHNICAL_DOC — micro-sv2

Engineer/contributor handbook. For *how to use it* see
[`USER_DOC.md`](USER_DOC.md). For terminology see
[`GLOSSARY.md`](GLOSSARY.md).

## 1. Architecture & Scope

- **Product/Service Name:** `micro-sv2` (workspace) / `ii-sv2` (umbrella).
- **Problem Statement:** Provide a small, embedded-friendly Stratum V2
  stack with end-to-end functional parity for elementary mining vs the
  reference implementation at
  `https://gitlab.ii.zone/redacted/`.
- **Story Mapping:** `CONN001`–`CONN004`, `CHAN001`–`CHAN002`,
  `MINE001`–`MINE003`, `LIB001`–`LIB004`. Deferred:
  `CHAN003`, `MINE004`.
- **Key Components** (layered top-down; each layer depends only on those
  below it):

  ```text
  ii-sv2          umbrella — re-exports types/framing/messages/security
  ├─ types        wire vocabulary (VarStr, VarBytes, Uint256, primitives)
  ├─ framing      Header, FrameView, FrameCodec trait + PlaintextCodec
  ├─ messages     9 elementary mining message types + dispatch enum
  └─ security     handshake, certs, EncryptedCodec
     └─ crypto    AEAD/secp256k1/noise primitives (internal)
  ```

  - **`types/`** — `Encode` / `Decode<'de>` traits, `Error` enum, and
    the borrowed-first wire vocabulary used by everything above.
    `no_std`, no external deps.
  - **`framing/`** — `Header` (6 bytes wire), `MessageDescriptor`,
    `FrameView<'a>`, `FrameCodec` trait + `PlaintextCodec` impl, the
    encode-side `Message` and `IntoFrame` traits, and the decode-side
    `MessageGroup<'de>` trait. Depends only on `types`.
  - **`messages/`** — single-file crate listing the 9 elementary mining
    messages, the `DeviceInfo` helper used inside `SetupConnection`,
    flag and error-code constants, and the `MiningMessages<'a>`
    dispatch enum. Depends on `types` + `framing`.
  - **`crypto/`** — internal crypto primitives wrapping `secp256k1`,
    `sha2`, `hkdf`, `chacha20poly1305`. Provides `HandShake`,
    `TransportState`, `KeyGen`. **Not** exposed via the umbrella crate.
  - **`security/`** — bridge crate combining noise + framing into the
    user-facing handshake driver and `EncryptedCodec`. Depends on
    `crypto`, `framing`, `types`.
  - **`ii-sv2/`** — pure re-export crate. Module-grouped: each
    submodule (`types`, `framing`, `messages`, `security`) re-exports
    `pub use ii_sv2_X::*;`. Has one doc-test that smoke-tests the
    re-export structure end-to-end.
  - **`interop-test/`** — excluded from the default workspace
    (`exclude = ["interop-test"]`); pulls a proprietary registry
    dep that we don't want in the default cargo motion. Run on demand
    via `cargo test --manifest-path interop-test/Cargo.toml`.

- **High-level data flow** (encrypted send path):

  ```text
  Message struct (e.g. SetupConnection)
       │
       │  IntoFrame::header() / body_len() / write_body()
       ▼
  EncryptedCodec::encode(frame, out)
       │
       │  1. plaintext header → out[..6]
       │  2. AEAD-seal out[..22]            ← TransportState::write_message_inplace
       │  3. plaintext body → out[22..N]    ← frame.write_body()
       │  4. AEAD-seal out[22..N+16]        ← TransportState::write_message_inplace
       ▼
  wire bytes (single contiguous buffer)
  ```

  Receive is the symmetric reverse, in two phases (decrypt header,
  parse `msg_length`, decrypt body).

## 2. Build & Tooling

| Area | Command | Notes |
|------|---------|-------|
| Workspace build | `cargo build` | Builds the 5 default workspace members; `interop-test` is excluded. |
| Per-crate build | `cargo build -p ii-sv2-types` (etc.) | Useful when iterating on one crate. |
| Tests | `cargo test` | Runs every workspace crate's tests. |
| Per-crate tests | `cargo test -p ii-sv2-security` | Faster iteration for crate-specific work. |
| Lints | `cargo clippy --all-targets -- -D warnings` | Required clean before commit. |
| Formatting | `cargo fmt` | Standard rustfmt. |
| Docs | `cargo doc --no-deps` | The umbrella's rustdoc page is the consumer landing page. |
| Interop tests (optional) | `cargo test --manifest-path interop-test/Cargo.toml` | Cross-implementation tests against the reference impl. |

- **Toolchain:** Rust **stable**, edition 2024. No nightly features
  required.
- **Environment variables:** none; the library has no runtime config.
- **Secrets:** none; tests use ephemeral keypairs generated locally.
- **Local stacks / containers:** none. Tests run in-process via
  `tokio::io::duplex` for the handshake/session loopback.

### Workspace metadata

`/Cargo.toml`:

```toml
[workspace]
resolver = "3"
members = ["crypto", "framing", "messages", "security", "types", "ii-sv2"]
exclude = ["interop-test"]

[workspace.dependencies]
serde            = { version = "1",   default-features = false, features = ["derive"] }
secp256k1        = { version = "0.31", default-features = false }
sha2             = { version = "0.10", default-features = false }
hkdf             = { version = "0.12", default-features = false }
chacha20poly1305 = { version = "0.10", default-features = false }
bs58             = { version = "0.5",  default-features = false, features = ["check"] }
embedded-io-async = { version = "0.6", default-features = false }
```

`serde` is in `[workspace.dependencies]` for historical reasons but
**no production crate currently depends on it** — `types`/`framing`/
`messages`/`security` were intentionally migrated to a hand-rolled
codec pair. The workspace entry can be retired once we confirm no
crate (including future ones) needs it.

## 3. Testing Strategy

The library has only unit and module-internal integration tests at
this stage; no end-to-end runbook yet. Test totals at the time of
writing:

| Crate | Test count | Coverage focus |
|-------|------------|----------------|
| `types` | 55 | Per-primitive round-trips, length-prefix dispatch, `OutOfBounds` / `Eof` paths. |
| `framing` | 35 | Header bit-packing, `FrameView::try_new` length validation, `MessageGroup` dispatch errors, `frame_to_message` happy + error paths, byte-exact wire spot checks. |
| `messages` | 15 | Per-message round-trip via `PlaintextCodec`, `Option<u32>` future-job round-trip, dispatch through `MiningMessages`, byte-exact `SetupConnectionSuccess` spot check. |
| `crypto` | ~10 | AEAD seal/open, secp256k1 keygen/sign, HKDF, noise handshake. |
| `security` | 17 unit + 2 integration | Handshake loopback, paired-codec fixture, single message round-trip, sequential frames (AEAD nonce advancement), `ChunkingNotImplemented` overflow guard, dispatch via `MiningMessages`. |
| `ii-sv2` | 1 doc-test | End-to-end smoke through the re-export structure (encode → decode `SetupConnectionSuccess`). |

Running `cargo test` on the default workspace exercises ~134 tests in
total. CI must keep this clean.

### Conventions

- Tests use stack-allocated `[u8; N]` buffers. No `alloc` in tests
  beyond the standard library (`std::vec!` etc. are fine inside
  `#[cfg(test)]`).
- Async tests in `security` use `#[tokio::test]` with the
  `embedded-io-adapters::tokio_1::FromTokio` shim — same pattern as
  the existing `handshake_loopback`.
- Test fixtures (e.g. `Dev` keygen, `paired_codecs` async helper)
  duplicate setup across crates rather than extracted into a shared
  test crate — Rust's default test-visibility model prefers
  per-crate duplication over a shared `dev-dependency`. Acceptable.
- Per-message round-trip tests use the same template across all
  9 messages in `messages/src/lib.rs`; the duplication is structural,
  not a smell.

## 4. Deployment & Operations

- **Pipelines:** none configured at this layer (this is a library, not
  a service). Consuming applications drive their own CI/CD.
- **Target environments:** any platform with a Rust 2024 toolchain.
  Embedded targets supported via `no_std`.
- **Release process:** when a versioned release is needed, bump
  `version` in each crate's `Cargo.toml` (workspace-coordinated
  manually) and tag the resulting commit. No registry publishing
  pipeline yet — consumers depend via git path.
- **Rollback / hotfix:** standard git revert of the offending commit;
  consumers update their `git` dependency reference.
- **Observability:** none provided by the library. Consumers
  instrument as needed.

## 5. Mapping to User Stories

Every implemented story maps to exactly one or two implementation
sites. Tests for each story live alongside the implementation.

| Story | Implementation | Tests |
|-------|----------------|-------|
| `CONN001` | `security/src/handshake.rs::{initiate, accept, EstablishedSession}` | `security/src/handshake.rs::tests::handshake_loopback` |
| `CONN002` | `security/src/{certificate,authenticator}.rs` | `security/src/{certificate,authenticator}.rs::tests` (multi-test) |
| `CONN003` | `security/src/session.rs::EncryptedCodec` | `security/src/session.rs::tests::{paired_codecs_fixture_builds, roundtrip_single_message, sequential_frames_advance_state, encode_rejects_oversized_payload, dispatch_via_mining_messages}` |
| `CONN004` | `messages/src/lib.rs::{SetupConnection, SetupConnectionSuccess, SetupConnectionError}` | `messages::tests::{setup_connection_roundtrip, setup_connection_success_roundtrip, setup_connection_success_byte_exact, setup_connection_error_roundtrip}` |
| `CHAN001` | `messages/src/lib.rs::OpenStandardMiningChannel` | `messages::tests::open_standard_mining_channel_roundtrip` |
| `CHAN002` | `messages/src/lib.rs::{OpenStandardMiningChannelSuccess, OpenMiningChannelError}` | `messages::tests::{open_standard_mining_channel_success_roundtrip, open_mining_channel_error_roundtrip}` |
| `MINE001` | `messages/src/lib.rs::NewMiningJob` | `messages::tests::{new_mining_job_roundtrip_with_min_ntime, new_mining_job_roundtrip_future_job, dispatch_new_mining_job}` |
| `MINE002` | `messages/src/lib.rs::SetNewPrevHash` | `messages::tests::set_new_prev_hash_roundtrip` |
| `MINE003` | `messages/src/lib.rs::SetTarget` | `messages::tests::set_target_roundtrip` |
| `LIB001` | `#![no_std]` declared at the root of every production crate | `cargo build` succeeds without `alloc` features |
| `LIB002` | All `Encode`/`Decode<'de>` impls; byte-exact spot checks | `framing::codec::tests::encode_msg_byte_exact`, `messages::tests::setup_connection_success_byte_exact`, `interop-test/` (out of default workspace) |
| `LIB003` | `ii-sv2/src/lib.rs` umbrella with module-grouped re-exports | `ii-sv2`'s doc-test |
| `LIB004` | `docs/GLOSSARY.md` + reviewer convention | (no automated test; reviewer responsibility) |

## 6. Additional References

- [`USER_DOC.md`](USER_DOC.md) — consumer-facing usage guide.
- [`GLOSSARY.md`](GLOSSARY.md) — terminology source of truth.
- Stratum V2 specification:
  <https://github.com/stratum-mining/sv2-spec/>
- Reference implementation mirrored by this workspace:
  <https://gitlab.ii.zone/redacted/>

> Keep this document evergreen: update commands, component lists,
> story mappings, and test counts whenever behavior or tooling changes.
> Reference other markdown files instead of duplicating content to
> avoid drift.
