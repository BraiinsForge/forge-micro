# GLOSSARY

Single source of truth for terminology used across the `micro-sv2`
codebase, documentation, and discussions. Public types, function
signatures, and API surface should align with the names defined here.
Local variables and implementation details are exempt.

When introducing a new domain concept in code or docs, check this
glossary first; if a term already exists, reuse it. Adding a new entry
requires updating this file in the same change set.

## 1. Domain Concepts

| Term | Definition | Code Convention | Anti-patterns |
|------|------------|-----------------|---------------|
| Stratum V2 | The protocol the workspace implements; a binary, encrypted, channel-based mining protocol | `Sv2`, `sv2`, `SV2` (in docs/abbreviations) | `stratum2`, `s2`, `stratum_v2` |
| Channel | An ordered, identified flow of mining work between a client and a server, scoped by `channel_id` | `channel`, `ChannelId` | `connection_id`, `session_channel` |
| Standard Channel | A mining channel where the pool computes the merkle root and the client only iterates the nonce | `standard_channel` | `simple_channel`, `basic_channel` |
| Frame | One Stratum V2 wire unit: a 6-byte header plus a length-prefixed payload | `Frame`, `FrameView` | `Packet`, `Message` (used for higher-level concept) |
| Header | The 6-byte SV2 frame header carrying `extension_type`, `msg_type`, `is_channel_message`, `msg_length` | `Header` | `FrameHeader`, `MsgHeader` |
| Message | A typed Stratum V2 protocol value (`SetupConnection`, `NewMiningJob`, ...) that maps to a frame's payload | `Message` (trait), per-struct names like `SetupConnection` | `Cmd`, `Packet`, `Datagram` |
| Extension | A namespace of related messages identified by `extension_type` (e.g. mining = `0x00`) | `extension_type`, `ExtType`, `MINING_EXTENSION_TYPE` | `protocol_id`, `kind` |
| Job | A unit of mining work referenced by `job_id`, distributed via `NewMiningJob` | `job_id`, `NewMiningJob` | `task`, `work_unit` |
| Share | A submission of work meeting a target threshold | `SubmitSharesStandard`, `SubmitSharesSuccess`, `SubmitSharesError` | `SubmitWork`, `Submission` |
| Target | A 256-bit difficulty threshold; valid hashes must be ≤ target | `target`, `max_target`, `Uint256` | `difficulty_threshold`, `nbits` (distinct concept), `goal` |
| nBits | Compact 32-bit encoding of difficulty used in block headers | `nbits` | `bits`, `compact_target` |
| PrevHash | The previous block hash distributed via `SetNewPrevHash` to redirect mining onto a new chain tip | `prev_hash`, `SetNewPrevHash` | `previous`, `parent_hash`, `tip` |
| Merkle root | The 256-bit root of the coinbase + transaction merkle tree, included in `NewMiningJob` | `merkle_root` | `tx_root`, `mtree_root` |
| Extranonce / Extranonce prefix | Vendor-allocated bytes prepended to the coinbase to extend miner search space | `extranonce`, `extranonce_prefix` | `nonce_extension`, `coinbase_suffix` |

## 2. Actors & Roles

| Actor | Definition | Scope | Notes |
|-------|------------|-------|-------|
| Miner | A device or proxy that opens a mining channel and consumes jobs to produce shares | Stratum V2 protocol, `client` side of the handshake | Initiator role; `initiate()` in `security` |
| Pool | The server distributing jobs and accepting shares | Stratum V2 protocol, `server` side of the handshake | Responder role; `accept()` in `security` |
| Proxy | An intermediary that may aggregate hashrate, transform jobs, or relay frames between miners and pools | Stratum V2 protocol | Re-emits `FrameView` via `IntoFrame` direct impl |
| Library consumer | Application or service author depending on `ii-sv2` (the umbrella) or any sub-crate directly | Rust API surface | Audience of `docs/USER_DOC.md` |
| Contributor | Engineer (or LLM agent) modifying the workspace itself | Build, tests, docs | Audience of `docs/TECHNICAL_DOC.md` |

## 3. Technical Terms

| Term | Definition | Related Concepts | Usage Example |
|------|------------|------------------|---------------|
| Borrowed-first | API style where types hold `&'a [u8]` / `&'a str` references into a caller-owned buffer rather than allocating | Zero-copy, `no_std`, `'a` lifetime | `pub struct Str0_255<'a>(&'a str)` |
| `no_std` | Rust crate attribute that excludes the standard library; targets bare-metal and constrained devices | `core`, `alloc` (also avoided here) | `#![no_std]` at crate root |
| Wire format | The byte layout of a value when transmitted; little-endian primitives, length-prefixed strings/bytes | `Encode` / `Decode<'de>` traits | `Header` is 6 bytes on the wire |
| AEAD | Authenticated Encryption with Associated Data; here `ChaCha20Poly1305` per noise spec | `MAC_SIZE`, sealing/opening | `transport.write_message_inplace(buf, len)` |
| Noise handshake | The Noise NX pattern that establishes paired `TransportState`s between initiator and responder | `EstablishedSession`, `Certificate` | See `security/src/handshake.rs` |
| Noise frame | A single AEAD chunk with up to 65 519 bytes of plaintext + 16-byte MAC | `MAX_PLAINTEXT_LENGTH`, chunking | Single-chunk MVP only — no chunking yet |
| Two-AEAD-chunks layout | The SV2 wire layout per encrypted frame: `enc(Header) | MAC | enc(payload) | MAC` | `EncryptedCodec`, `HEADER_WIRE_SIZE = 22` | Hidden behind `FrameCodec::encode`/`decode_*` |
| `FrameCodec` | The trait abstracting wire-level encode/decode; impls: `PlaintextCodec` (no transformation), `EncryptedCodec` (noise-sealed) | `encode`, `decode_header`, `decode_frame`, `payload_wire_size` | `framing/src/codec.rs` |
| `Encode` / `Decode<'de>` | The byte-level codec trait pair from `ii-sv2-types`; used in place of a `serde`-based model | Used by every type in `types`/`framing`/`messages` | `framing/src/codec.rs` (defined in `types`) |
| `IntoFrame` | Trait describing anything the codec can encode as a frame (`Message`s and `FrameView`s have impls) | `Message`, `FrameView` | `pub trait IntoFrame { fn descriptor(&self) -> _ ; ... }` |
| `Message` | Per-message-struct trait carrying the static `MessageDescriptor` for that wire identity | `MessageDescriptor`, `IntoFrame` (blanket impl) | `impl Message for SetupConnection<'_>` |
| `MessageGroup<'de>` | Per-extension-enum trait for dispatching a parsed `FrameView` into a typed variant by `(msg_type, is_channel_message)` | `MiningMessages`, `dispatch` | `impl<'de> MessageGroup<'de> for MiningMessages<'de>` |
| `FrameView<'a>` | A borrowed view of a complete frame: `Header` + payload slice | `try_new`, `MessageGroup::dispatch` | Returned by `decode_frame` |
| `MessageDescriptor` | A `(extension_type, msg_type, is_channel_message)` triple identifying a message kind without its length | `Message::DESCRIPTOR` | Used by `frame_to_message` and `dispatch` |
| Channel-message bit | The high bit of the 16-bit `extension_field` in a `Header`, indicating the frame is scoped to a channel | `is_channel_message`, `Header::MAX_LEN` | Set on `NewMiningJob`, `SetNewPrevHash`, `SetTarget` |
| `Uint256<'a>` | A borrowed 256-bit little-endian value used for hashes and targets | `to_le_bytes`, `from_le_bytes` | `merkle_root: Uint256<'a>` |
| `VarStr<'a, MIN, MAX>` / `Str0_255<'a>` | Length-bounded UTF-8 strings, length prefix on the wire | `TryFrom<&'a str>`, `OutOfBounds` | `endpoint_host: Str0_255<'a>` |
| `VarBytes<'a, MIN, MAX>` / `Bytes0_32<'a>` | Length-bounded byte sequences, length prefix on the wire | `TryFrom<&'a [u8]>` | `extranonce_prefix: Bytes0_32<'a>` |
| Single-chunk MVP | The current implementation handles only frames where the body fits in one noise frame (≤ 65 519 bytes) | `ChunkingNotImplemented`, `MAX_PLAINTEXT_LENGTH` | Documented in `EncryptedCodec` |
| Loopback handshake | A test-only pattern that runs `initiate` + `accept` over an in-memory duplex pipe to produce paired transport states | `paired_codecs` fixture | See `security/src/handshake.rs::tests` |

## 4. Abbreviations & Acronyms

| Abbreviation | Full Form | Context |
|--------------|-----------|---------|
| SV2 | Stratum V2 | Protocol references |
| AEAD | Authenticated Encryption with Associated Data | Crypto / noise transport |
| MAC | Message Authentication Code | 16-byte trailer on each AEAD chunk |
| NX | Noise pattern: ephemeral both sides, static for responder, no static for initiator | Handshake pattern |
| MVP | Minimum Viable Product | Used for "single-chunk MVP" |
| HRTB | Higher-Ranked Trait Bound | Rust type system feature, occasional notes in code/docs |
| LE | Little-endian | Wire format primitive ordering |
| PoW | Proof of Work | Mining concept |
| BIP | Bitcoin Improvement Proposal | E.g. BIP320 referenced in `NewMiningJob.version` |

## 5. Maintaining the Glossary

### When to add new terms

- A new domain concept enters the system.
- A previously informal term needs a formal definition.
- An onboarding reader gets confused by overloaded usage.

### Process for updates

1. Propose the term in the change set's PR/commit message.
2. Confirm no synonym already exists in this file.
3. Add a complete entry: definition, code convention, anti-patterns.
4. Update any code/docs that used inconsistent naming in the same change set.

### Review checklist

- [ ] The term is necessary (not a synonym for an existing entry).
- [ ] Definition is unambiguous.
- [ ] Code convention is specified.
- [ ] Anti-patterns list common alternatives to avoid.
- [ ] Existing code/docs checked for consistency.

## 6. Documentation filename conventions

- Reserved root markdown files use uppercase: `README.md`,
  `IMPLEMENTATION_PLAN.md` (when present).
- Reserved Braiins documentation files use uppercase:
  `docs/USER_DOC.md`, `docs/TECHNICAL_DOC.md`, `docs/GLOSSARY.md`.
- All other markdown files use lowercase kebab-case
  (e.g. `runbook-template.md`).
