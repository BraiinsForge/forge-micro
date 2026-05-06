# USER_DOC — micro-sv2

Consumer guide for `micro-sv2`. Audience: Rust developers writing SV2
mining clients, pool servers, or proxies who want a small, embedded-
friendly library. For *how* it's built internally, see
[`TECHNICAL_DOC.md`](TECHNICAL_DOC.md).

## 1. Introduction & Audience

- **Product/Service Name:** `micro-sv2` (workspace) / `ii-sv2` (umbrella crate).
- **Who Should Read This:** Rust developers integrating Stratum V2 client,
  server, or proxy logic into an application.
- **What They Get:** A `no_std` Stratum V2 stack — wire types, framing,
  the elementary mining message set, and a noise-encrypted session — all
  reachable through one cargo dependency.
- **Related User Stories:** `CONN001`–`CONN004`, `CHAN001`–`CHAN002`,
  `MINE001`–`MINE003`, `LIB001`–`LIB003`.

## 2. Getting Started

### Prerequisites

- Rust **stable**, edition 2024.
- For tests / async use: a tokio (or any `embedded-io-async`-compatible)
  runtime. Library itself is async-runtime-agnostic.
- For embedded targets: a `no_std` toolchain. The library uses no
  allocator on the production path.

### Quick start (plaintext, no encryption)

Add the umbrella crate as a dependency:

```toml
[dependencies]
ii-sv2 = { git = "https://gitlab.ii.zone/dynamo/libraries/micro-sv2" }
```

Encode and decode a SV2 frame end-to-end without encryption (useful for
tests and simulators):

```rust
use ii_sv2::framing::{FrameCodec, PlaintextCodec, frame_to_message};
use ii_sv2::messages::SetupConnectionSuccess;

let original = SetupConnectionSuccess { used_version: 2, flags: 0 };
let mut codec = PlaintextCodec;

// Encode into a single contiguous buffer.
let mut wire = [0u8; 32];
let n = codec.encode(&original, &mut wire).unwrap();
debug_assert_eq!(n, 12); // 6-byte header + 6-byte body

// Receiving side: two reads — header first (fixed size), then payload.
let mut hbuf = [0u8; PlaintextCodec::HEADER_WIRE_SIZE];
hbuf.copy_from_slice(&wire[..PlaintextCodec::HEADER_WIRE_SIZE]);
let header = codec.decode_header(&mut hbuf).unwrap();

let pn = PlaintextCodec::payload_wire_size(header.msg_length as usize);
let mut pbuf = [0u8; 32];
pbuf[..pn].copy_from_slice(
    &wire[PlaintextCodec::HEADER_WIRE_SIZE..PlaintextCodec::HEADER_WIRE_SIZE + pn],
);
let frame = codec.decode_frame(header, &mut pbuf[..pn]).unwrap();
let received: SetupConnectionSuccess = frame_to_message(frame).unwrap();

assert_eq!(received, original);
```

### Reference environments

- **Local development:** the workspace tests run unmodified on Linux,
  macOS, and Windows. `cargo test` exercises every crate.
- **Production wire:** any TCP transport. Encrypt frames with
  `security::EncryptedCodec` after a noise handshake.
- **Embedded target:** verified `no_std` build — every production
  module compiles without `alloc`. Async I/O via `embedded-io-async`.

## 3. Feature Overview & Journeys

| Feature | Story IDs | Description | Entry Points |
|---------|-----------|-------------|--------------|
| Encrypted handshake | `CONN001`, `CONN002` | Run the SV2 noise NX handshake; produce paired `EstablishedSession`s | `security::initiate`, `security::accept` |
| Encrypted frame transport | `CONN003` | Send/receive frames over an established session, two AEAD chunks per frame | `security::EncryptedCodec` |
| Connection setup | `CONN004` | Negotiate protocol/version/flags before any channel | `messages::{SetupConnection*}` |
| Open standard channel | `CHAN001`, `CHAN002` | Request a channel from the pool; handle success or error | `messages::{OpenStandardMiningChannel*, OpenMiningChannelError}` |
| Receive jobs/prevhash/target | `MINE001`–`MINE003` | Decode and dispatch incoming mining frames | `messages::{NewMiningJob, SetNewPrevHash, SetTarget, MiningMessages}` |

### Journey: client connects to a pool

1. Open a TCP socket to the pool's hostname/port.
2. Wrap it in an `embedded-io-async` adapter.
3. Run `security::initiate(&mut socket, rng, authenticator).await` →
   yields an `EstablishedSession`.
4. Wrap the session: `let (mut codec, session_id) = EncryptedCodec::from_handshake(established);`.
5. Encode a `SetupConnection`; push bytes to the socket; pull bytes
   back; decode through `codec.decode_header` then `codec.decode_frame`.
6. Match the response against
   `MiningMessages::dispatch(frame)?` — typed variant in hand.

### Journey: pool accepts a miner

1. Accept a TCP connection.
2. Wrap it in `embedded-io-async`.
3. Run `security::accept(&mut socket, rng, &cert_with_priv).await` →
   yields an `EstablishedSession` (no `remote_identity` for NX
   responder).
4. Same `EncryptedCodec::from_handshake` and dispatch loop.
5. After receiving `SetupConnection`, send back `SetupConnectionSuccess`
   (or `SetupConnectionError` on rejection).
6. After receiving `OpenStandardMiningChannel`, send back
   `OpenStandardMiningChannelSuccess` or `OpenMiningChannelError` and
   start streaming `NewMiningJob` / `SetNewPrevHash` / `SetTarget` for
   the channel.

### Journey: forwarding a frame (proxy)

`FrameView<'a>` implements `IntoFrame`, so a parsed frame can be
re-emitted as-is without decoding the body:

```rust
use ii_sv2::framing::{FrameCodec, FrameView, frame_to_message};

# fn forward<C, D>(mut input: C, mut output: D) -> Result<(), C::Error>
# where
#     C: FrameCodec,
#     D: FrameCodec<Error = C::Error>,
# {
// Pull a frame off the input codec
let mut hbuf = [0u8; C::HEADER_WIRE_SIZE];
// ... read HEADER_WIRE_SIZE bytes from upstream into hbuf ...
let header = input.decode_header(&mut hbuf)?;
let pn = C::payload_wire_size(header.msg_length as usize);
let mut pbuf = [0u8; 4096];
// ... read pn bytes into pbuf[..pn] ...
let frame: FrameView<'_> = input.decode_frame(header, &mut pbuf[..pn])?;

// Re-emit to the downstream codec without re-decoding.
let mut out = [0u8; 4096];
let _n = output.encode(&frame, &mut out)?;
# Ok(())
# }
```

## 4. Integration & API Usage

### Authentication

SV2 noise NX:
- **Initiator** (miner): provides an `Authenticator` (typically
  `ValidateFull` in production). Validates the responder's certificate
  during the handshake.
- **Responder** (pool): owns a `CertificateWithPrivateKey` issued by
  an authority key. Presents it inside the encrypted handshake message.

### Configuration knobs

There are no runtime configuration files; the library is a Rust crate
configured by what you pass into its constructors.

Typical knobs from a downstream consumer:

- `Authenticator` choice (`ValidateNothing` for tests, `ValidateFull`
  for production).
- Certificate validity window (`CertificateHeader::valid_from` /
  `not_valid_after`).
- Buffer sizes you allocate for `decode_header` and `decode_frame`
  (depend on the largest message you expect; see [Operations](#5-operations--troubleshooting)).

### API examples

#### Encode and send a typed message

```rust
use ii_sv2::framing::FrameCodec;
use ii_sv2::messages::SetupConnection;

# fn send<C: FrameCodec>(mut codec: C, msg: SetupConnection<'_>, out: &mut [u8]) -> Result<usize, C::Error> {
let n = codec.encode(&msg, out)?;
// `out[..n]` is the wire bytes; push them through your transport.
# Ok(n)
# }
```

#### Decode an incoming frame and dispatch by type

```rust
use ii_sv2::framing::{FrameCodec, MessageGroup};
use ii_sv2::messages::MiningMessages;

# fn recv<'b, C: FrameCodec>(
#     codec: &mut C,
#     hbuf: &mut [u8],
#     pbuf: &'b mut [u8],
# ) -> Result<MiningMessages<'b>, C::Error> {
let header = codec.decode_header(hbuf)?;
let pn = C::payload_wire_size(header.msg_length as usize);
let frame = codec.decode_frame(header, &mut pbuf[..pn])?;
// `MiningMessages::dispatch` returns `FramingError`; the codec wraps it.
let msg: MiningMessages<'b> = MessageGroup::dispatch(frame).map_err(Into::into)?;
# Ok(msg)
# }
```

#### Decode an incoming frame into a known concrete type

When you know exactly which message you expect (e.g. you just sent a
request and you're awaiting *its* response):

```rust
use ii_sv2::framing::{FrameCodec, frame_to_message};
use ii_sv2::messages::SetupConnectionSuccess;

# fn await_setup<'b, C: FrameCodec>(
#     codec: &mut C,
#     hbuf: &mut [u8],
#     pbuf: &'b mut [u8],
# ) -> Result<SetupConnectionSuccess, C::Error> {
let header = codec.decode_header(hbuf)?;
let pn = C::payload_wire_size(header.msg_length as usize);
let frame = codec.decode_frame(header, &mut pbuf[..pn])?;
let success: SetupConnectionSuccess =
    frame_to_message(frame).map_err(Into::into)?;
# Ok(success)
# }
```

### Library import shapes

The umbrella crate uses module-grouped re-exports. Either of these
forms works (they point to the same items):

```rust
// Module-grouped (preferred — architecture is visible)
use ii_sv2::framing::{FrameCodec, Header, PlaintextCodec};
use ii_sv2::messages::{SetupConnection, MiningMessages};
use ii_sv2::security::{EncryptedCodec, EstablishedSession, accept, initiate};
use ii_sv2::types::{Str0_255, Uint256};

// Or via the deeper sub-module paths
use ii_sv2::security::session::EncryptedCodec;
```

If you want the bare implementation crates (e.g. you only need
`ii-sv2-types` for a small utility), depend on them directly — they are
all independently usable.

## 5. Operations & Troubleshooting

### Common errors

| Symptom | Resolution |
|---------|-----------|
| `SessionError::ChunkingNotImplemented { length }` on encode | The body exceeds 65 519 bytes. Elementary mining never produces frames this large; if you hit this, check that your `Message` impl's `encoded_len()` is correct. Multi-chunk noise framing is a future addition. |
| `SessionError::Crypto(_)` on decode | AEAD-open failed. Either the wire bytes were corrupted in transit (unlikely on TCP), or the sender/receiver `TransportState`s are out of sync (e.g. one side missed a frame). Drop the connection and re-handshake. |
| `FramingError::WrongMessage { expected, found }` | You used `frame_to_message::<M>(frame)` but the frame's descriptor was for a different message. Use `MiningMessages::dispatch(frame)` if you don't know what to expect. |
| `FramingError::WrongExtension { expected, found }` | Frame is from a different SV2 extension than `MiningMessages` covers. Either you're talking to a non-mining endpoint or a peer is misbehaving. |
| `FramingError::TruncatedPayload { announced, available }` | The byte count fed to `decode_frame` doesn't match `header.msg_length`. Recheck the buffer-fetch logic — typically a partial socket read. |
| `FramingError::OutOfBounds` (via `Body(_)`) | A `Str*` or `Bytes*` field's wire length was outside its declared `MIN..=MAX`. Sender's payload is malformed. |
| `InitializationError::Authentication(_)` during handshake | The responder's certificate failed validation — check authority keys and validity windows. |

### Sizing read buffers

You need two buffers per receive iteration:

- **Header buffer:** `[u8; CODEC::HEADER_WIRE_SIZE]`. 6 bytes for
  `PlaintextCodec`, 22 bytes for `EncryptedCodec`. Allocate once per
  session.
- **Payload buffer:** size depends on the largest frame you expect.
  For elementary mining, an 8 KiB stack buffer covers every in-scope
  message with margin (real bodies are 32–700 bytes). On embedded
  devices with tight memory, a 4 KiB buffer is sufficient.

### Monitoring hooks

The library does not emit metrics or logs of its own. Instrumentation
is the consumer's responsibility — wrap codec calls in your own
counters/timers, or implement a thin tracing layer over the
`FrameCodec` trait.

## 6. References & Change Log

- [`TECHNICAL_DOC.md`](TECHNICAL_DOC.md) — implementation details,
  build/test workflows.
- [`GLOSSARY.md`](GLOSSARY.md) — terminology.
- Rustdoc landing page: `cargo doc -p ii-sv2 --open`.
- Stratum V2 specification: <https://github.com/stratum-mining/sv2-spec/>
- Reference implementation:
  <https://gitlab.ii.zone/redacted/>

When library behavior changes (new public types, error variants,
trait bounds), update this document alongside the code.
