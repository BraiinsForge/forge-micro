# micro-sv2

Borrowed-first, `no_std` Stratum V2 reference implementation, packaged for
constrained mining devices.

The four-crate stack (`ii-sv2-types`, `ii-sv2-framing`, `ii-sv2-messages`,
`ii-sv2-security`) plus the umbrella `ii-sv2` crate cover the elementary
SV2 mining flow end-to-end: noise-encrypted handshake, framed message
transport, and the message types needed to set up a connection, open a
standard channel, and receive jobs / prev-hash / target updates from a pool.

## Quick start

```toml
[dependencies]
ii-sv2 = { path = "path/to/ii-sv2" }
```

```rust
use ii_sv2::framing::{FrameCodec, PlaintextCodec, frame_to_message};
use ii_sv2::messages::{MiningMessages, SetupConnectionSuccess};

// Build a message and encode it to the wire.
let original = SetupConnectionSuccess { used_version: 2, flags: 0 };
let mut codec = PlaintextCodec;
let mut buf = [0u8; 32];
let n = codec.encode(&original, &mut buf).unwrap();

// On the receiving side, two reads: header first (fixed size), then payload.
let mut hbuf = [0u8; PlaintextCodec::HEADER_WIRE_SIZE];
hbuf.copy_from_slice(&buf[..PlaintextCodec::HEADER_WIRE_SIZE]);
let header = codec.decode_header(&mut hbuf).unwrap();

let pn = PlaintextCodec::payload_wire_size(header.msg_length as usize);
let mut pbuf = [0u8; 32];
pbuf[..pn].copy_from_slice(
    &buf[PlaintextCodec::HEADER_WIRE_SIZE..PlaintextCodec::HEADER_WIRE_SIZE + pn],
);
let frame = codec.decode_frame(header, &mut pbuf[..pn]).unwrap();
let received: SetupConnectionSuccess = frame_to_message(frame).unwrap();
assert_eq!(received, original);
```

For encrypted production use, swap `PlaintextCodec` for
`security::EncryptedCodec` (built from a completed noise handshake — see
`docs/USER_DOC.md`).

## Status

Functionally complete for **elementary mining over standard channels**.
The four-crate stack ships 134+ passing tests including a handshake
loopback and end-to-end frame round-trips.

Out of scope (for now): extended/group channels, share submission, job
declaration, template distribution, channel housekeeping.

## Architecture

```
ii-sv2          umbrella — single dep entry point
├─ types        wire vocabulary (VarStr, VarBytes, Uint256, ...)
├─ framing      Header, FrameView, FrameCodec trait, PlaintextCodec
├─ messages     9 elementary mining message types + dispatch enum
└─ security     handshake, certs, EncryptedCodec
   └─ crypto    AEAD/secp256k1/noise primitives (internal layer)
```

Layering rule: each crate depends only on crates *above* it in the diagram.
The umbrella crate is a thin re-export shim — no original logic.

## Documentation

- [`docs/USER_DOC.md`](docs/USER_DOC.md) — consumer guide: getting started, integration, API examples, troubleshooting.
- [`docs/TECHNICAL_DOC.md`](docs/TECHNICAL_DOC.md) — architecture, build/test workflow, crate map.
- [`docs/GLOSSARY.md`](docs/GLOSSARY.md) — domain terminology, actor definitions, abbreviations.

## License

This program is free software licensed under the terms of the GNU
General Public License, version 3 or (at your option) any later version.
See [`COPYING`](../COPYING) for the full text.

Braiins Systems s.r.o. reserves the right to license this program, or
any part thereof, under separate terms.
