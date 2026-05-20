# Mining Protocol Abstraction

## Goal

Add Stratum V2 support to the on-device mining client without forking
the integration layer. Both V1 and V2 should drive the same Embassy
task, talk to the same `AsicSession`, and share the same job/work
rings in `stratum-client` (the integration crate that replaces
`ubos-backend`). The shared interface between adapters and
`stratum-client` is a small `no_std` trait crate; each protocol gets
its own adapter.

## Non-goals

- SV2 protocol implementation. That lives in `~/braiins/micro-sv2/`,
  a separate repository providing types, framing, messages, and
  crypto. This document describes only how `eth-examples` consumes it.
- New CPU solver behavior. Today's "always-zero extranonce_2" stays.
  The trait shape supports rolling but the implementation comes with
  the ASIC driver.

## Crate layout

```
eth-examples/
  mining-protocol/    NEW — trait + borrowed domain types, no_std, no alloc
  stratum-v1/         restructured: V1 wire types + MiningClient impl
                      (renamed from stratum-v1-core)
  stratum-v2/         NEW — V2 adapter, MiningClient impl, depends on micro-sv2
  stratum-client/     NEW — replaces ubos-backend; generic over MiningClient
  asic-cpu-mock/      NEW — ASIC-side of AsicSession for the CPU mock
  asic-uart-peer/     NEW — ASIC-side of AsicSession over UART to a peer
  eth-examples-host/  NEW — embassy-net + tuntap integration harness
  # ubos-backend, cpu-miner, asic-interface deleted: all firmwares
  # have migrated and the legacy stack is no longer used.
```

External:

- `~/braiins/micro-sv2/` — owns its types, framing, messages, and
  crypto, including the Noise handshake. Has no awareness of
  `eth-examples`.

### Why one crate per protocol, not two

V1 ends up as one crate (`stratum-v1`) that holds both the wire
layer and the `MiningClient` impl. V2 ends up as one adapter crate
(`stratum-v2`) that consumes the wire layer from external
`micro-sv2`. Symmetric: one adapter crate per protocol.

A two-crate split for V1 (separate adapter crate on top of a
wire-only `stratum-v1-core`) was considered and rejected. The
adapter has only one consumer (`stratum-client`); splitting forces
several internal types to be `pub` purely for cross-crate
visibility. The wire-vs-adapter separation is preserved as a
module boundary inside the crate, not a crate boundary.

### Internal layout of `stratum-v1`

```
src/
  wire/                  V1 wire types and codecs
    messages.rs            existing: typed messages
    parse.rs               existing: parser
    rpc.rs                 existing: Rpc envelope and Method enum
    line_buffer.rs         existing
  client.rs              new: MiningClient impl, state machine
  state.rs               new: session state — extranonce, mask, JobId map
  scratch.rs             new: typed decode_scratch struct
  hex.rs                 new: hex decode utility (moved from ubos-backend)
  error.rs               existing
  lib.rs                 re-exports
```

`stratum_v1::wire::*` stays usable standalone. The existing
`examples/probe.rs` continues to test the wire layer against a real
pool. A new `examples/client_probe.rs` exercises the
`MiningClient` impl end-to-end in std mode.

### V1 wire-types alignment with V2

V1 wire types are already borrow-first (`Notify<'a>` borrows hex
strings from `LineBuffer`). The structural alignment with V2 is
limited to API shape:

| Step | V2 (micro-sv2) | V1 (stratum-v1) |
|---|---|---|
| Wire bytes | `wire_buffer` (encrypted) | `LineBuffer` (plain JSON line) |
| In-place transform | AEAD decrypt | none |
| Borrowed wire types | `NewMiningJob<'a>` (binary) | `Notify<'a>` (hex `&str`) |
| To-binary step | nothing — already binary | hex-decode into `decode_scratch` |
| Trait surface | `MiningJob<'_>` borrowing wire_buffer | `MiningJob<'_>` borrowing decode_scratch |

Both end up with the same shape at the trait boundary. V1 needs
`decode_scratch` because hex bytes have to become binary somewhere.

### Restructuring expected during the V1 adapter work

The wire types themselves keep their hex-`&str` shape. The changes:

- **`wire/encode.rs`** — new `Encode` trait mirroring `ii_sv2_types::Encode`,
  plus per-message impls (`Subscribe::encode_to(&self, &mut [u8]) -> Result<usize, _>`,
  etc.). Replaces today's standalone `encode_subscribe`/`encode_authorize`
  functions. Lets the adapter call `msg.encode_to(buf)` symmetrically with V2.
- **`wire/dispatch.rs`** — new `V1Message<'a>` flat enum
  (`{ Notify(_), SetDifficulty(_), SubscribeResult(_), AuthorizeResult(_), ... }`)
  replacing today's nested `Rpc<'a> { Response, Notification(_) }`. Aligns
  with V2's `MiningMessages<'a>` so dispatch looks identical at the call site.
- **`hex.rs`** — hex decode utility (`decode_hex` currently in `ubos-backend`)
  moves in.
- **`scratch.rs`** — typed `DecodeScratch` struct (binary `prev_hash[32]`,
  `coinbase_prefix[…]`, `coinbase_suffix[…]`, `branches: [[u8; 32]; 16]`,
  precomputed `merkle_root[32]`). Filled inside `accept()`; `MiningJob<'_>`
  fields point into it.
- **Merkle root compute** moves into `mining-protocol` as
  `LazyMerkle::compute`. V2 standard channels never use the `Prepared`
  arm, but the type owns the operation.
- **No `LineAction::Notify`-style intermediate.** Today's `ubos-backend`
  copies parsed strings into a ~1.6 KiB enum because parse and dispatch
  happen in different scopes. The visitor pattern keeps the borrow live
  for the whole `accept()` call, so the adapter parses → hex-decodes into
  `decode_scratch` → builds `MiningJob<'_>` → calls visitor — all in one
  borrow scope, no copy.
- **Visibility tweaks** where `client.rs` needs to read parser internals
  that are currently private to `parse.rs`.

## Trait API

`mining-protocol` exposes one trait pair plus a small set of borrowed
domain types. Sketch:

```rust
pub trait MiningClient {
    /// Kick off protocol-transport setup. For V1 this is a no-op; the
    /// configure/subscribe/authorize messages are queued for the next
    /// drive(). For V2 this performs the Noise handshake on the wire
    /// and queues SetupConnection + OpenStandardMiningChannel for the
    /// next drive(). Returns Err only on transport-level failures
    /// (Noise mismatch, signature validation failure).
    async fn connect<T>(&mut self, transport: &mut T) -> Result<(), Error>
    where T: Read + Write;

    /// Sync — encode and queue a share submission. No I/O.
    fn submit(&mut self, share: &Share<'_>) -> Result<(), Error>;

    /// Async, cancellation-safe. Flushes outgoing, reads incoming,
    /// decrypts in place, returns when one event is ready for accept().
    async fn drive<T>(&mut self, transport: &mut T) -> Result<(), Error>
    where T: Read + Write;

    /// Sync — dispatch the most recently received event.
    fn accept<V: MiningClientVisitor>(&mut self, visitor: &mut V);
}

pub trait MiningClientVisitor {
    fn visit_new_job(&mut self, job: MiningJob<'_>);
    fn visit_submit_result(&mut self, result: SubmitResult<'_>);
    fn visit_session_update(&mut self, update: SessionUpdate<'_>);
}
```

Lifetimes on visitor arguments are bound to `&mut self` on the client
for the duration of `accept`. The visitor must copy out anything it
needs to keep.

### Initial session setup flows through the visitor

There is no separate "session established" return type. After
`connect()` returns, the caller enters the regular `drive()` /
`accept()` loop. The first few cycles deliver the initial session
parameters as a sequence of `SessionUpdate` events — the same shape
used for mid-session reconfiguration.

For V1 the initial sequence is roughly:
- configure response → `SetVersionMask`
- subscribe response → `SetExtranonce`
- authorize response → no event on success; `Err` from `drive` on
  rejection (session unusable, caller reconnects)
- first `mining.set_difficulty` → `SetTarget`
- first `mining.notify` → `NewJob`

For V2 standard channels:
- `SetupConnection.Success` flags → `SetVersionMask`
- `OpenStandardMiningChannel.Success` → `SetExtranonce` + `SetTarget`
- `SetupConnection.Error` / `OpenChannel.Error` → `Err` from `drive`
- first `NewMiningJob` (with prev_hash from `SetNewPrevHash`) →
  `NewJob`

The receiver (`stratum-client`'s visitor impl) does not need to
track "is initial setup done?" — it simply waits for the first
`visit_new_job` before the work generator starts emitting
`WorkAssignment`s into the `AsicSession`. Until then session state
accumulates from `visit_session_update` calls.

### SessionUpdate

```rust
pub enum SessionUpdate<'a> {
    SetTarget(&'a [u8; 32]),
    SetExtranonce { prefix: &'a [u8], en2_size: u8 },
    SetVersionMask(u32),
    Reconnect { host: &'a str, port: u16, wait_seconds: u16 },
}
```

V1 adapter emits all four variants — both during initial handshake
and mid-session. V2 standard channels emit `SetTarget`,
`SetExtranonce`, `SetVersionMask` during initial setup, then
`SetTarget` and `Reconnect` mid-session; the rest V2 standard does
not produce mid-session.

## Borrowed domain types

All wire data is exposed in **binary** form. Hex is a wire-encoding
detail and stays inside the V1 adapter.

### Byte-order contract at the trait boundary

The trait promises **canonical, ready-to-use** byte order for every
field. Adapters are responsible for whatever conversion the wire
form requires.

| Field | Trait form | V1 wire | V2 wire |
|---|---|---|---|
| `prev_hash` | block-header byte order — direct memcpy into `header[4..36]` | hex, with **per-u32 byte swap** (see bos-main `v1.rs:127`) | binary, natural order |
| `merkle_root` (in `LazyMerkle::Final` / computed from `Prepared`) | block-header byte order | wire is hex, no swap | binary, natural order |
| `target` | little-endian 256-bit integer — `target[0]` is LSB, `target[31]` is MSB | derived from float diff via `target_from_diff` | binary, per SV2 spec |
| `version`, `ntime`, `nbits` | native `u32` | hex (big-endian text) parsed to `u32` | wire `u32` |
| `coinbase_prefix`, `coinbase_suffix`, `extranonce_prefix`, merkle branches | raw byte slices, no swap | hex, no swap | binary, no swap |

The "brain-damaged" V1 prev_hash byte swap moves into the V1 adapter's
hex-decode step. The block header builder in `asic-cpu-mock` (and
the `WorkAssignment` shape generally) becomes a straight
`header[4..36].copy_from_slice(&prev_hash)` — no per-word loop, no
comment about V1 quirks. The solver never sees a V1-specific
byte order.

This is the main reason the trait is worth having even before V2
lands: it pulls protocol-specific byte gymnastics out of the solver
and pushes them into the adapter where they belong.

```rust
pub struct MiningJob<'a> {
    pub id: JobId,
    pub prev_hash: &'a [u8; 32],
    pub version: u32,
    pub version_rolling_mask: u32,
    pub nbits: u32,
    pub ntime: u32,
    pub target: &'a [u8; 32],
    pub clean_jobs: bool,
    pub extranonce_2_size: u8,    // V2 standard: 0; V1: per session
    pub merkle: LazyMerkle<'a>,
}

pub struct Share<'a> {
    pub id: JobId,
    pub nonce: u32,
    pub ntime: u32,
    pub version_bits: u32,
    pub extranonce_2: &'a [u8],   // V1: rolled bytes; V2 standard: &[]
}
```

### LazyMerkle

The V1 / V2 merkle root semantic difference is encapsulated here.

```rust
pub enum LazyMerkle<'a> {
    Prepared {
        extranonce_prefix: &'a [u8],
        coinbase_prefix: &'a [u8],
        coinbase_suffix: &'a [u8],
        branches: &'a [[u8; 32]],
    },
    Final(&'a [u8; 32]),
}

impl LazyMerkle<'_> {
    pub fn compute(&self, extranonce_2: &[u8], out: &mut [u8; 32]);
}
```

V1 adapter populates `Prepared` from decoded coinbase parts. V2
standard adapter populates `Final` directly from `NewMiningJob`.
Solvers call `compute(extranonce_2, &mut root)` regardless of variant;
`Final` ignores `extranonce_2`. The size signal is `extranonce_2_size`,
which is `0` for V2 standard.

### JobId

```rust
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub struct JobId(pub u32);
```

The adapter assigns a fresh `JobId` for each `visit_new_job` and keeps
a depth-2 ring `[(JobId, wire_id); 2]` for translation back to the
wire form on submit. Ring depth matches `stratum-client`'s job ring;
the two rotate in lockstep.

V1 wire id is `heapless::String<MAX_JOB_ID_CHARS>`. V2 wire id is
`u32`. `stratum-client` only sees `JobId` and never touches either
form.

defmt logs the wire form once per notify (`v1: generic=N wire=...`)
so an operator can correlate `JobId(N)` lines to pool-side logs.

## Two-buffer scheme inside each adapter

Each adapter holds two buffers:

- **wire_buffer** — raw frame from the network. V1: one JSON-RPC line.
  V2: one SV2 frame, decrypted in place.
- **decode_scratch** — typed slots for binary derivations.

For V1, `decode_scratch` carries hex-decoded `prev_hash`, coinbase
parts, branches, and the merkle root. Borrowed slices in
`MiningJob<'a>::merkle` (`Prepared` variant) point into it.

For V2 standard channels, `decode_scratch` is mostly unused.
`NewMiningJob` carries the merkle root directly; `MiningJob<'a>`
slices point into the decrypted plaintext in `wire_buffer`.

Both buffers are reset every drive cycle. State that genuinely
persists across messages (V1 extranonce_1, configure-negotiated mask;
V2 noise transport keys/nonces, channel_id) lives as plain fields on
the adapter struct, not in a buffer.

`decode_scratch` is a typed struct, not a bump arena. Predictable
size, no `unsafe`, no fragmentation.

## stratum-client architecture

`stratum-client` is the integration crate. It owns the `MiningClient`
adapter (V1 or V2 — generic), the visitor impl, two rings, the work
generator, and the `AsicSession` shared with the ASIC implementation.
See `STRATUM_ARCHITECTURE.md` for the visual.

### Job ring (depth 2) with validity flag

```rust
struct JobRing<const N: usize> {
    slots: [JobSlot; N],
    write_index: usize,
}

struct JobSlot {
    job_id: JobId,
    valid: bool,
    prev_hash: [u8; 32],
    target: [u8; 32],
    merkle_root: [u8; 32],   // precomputed at visit_new_job
    version: u32,
    version_mask: u32,
    nbits: u32,
    ntime_base: u32,
    ntime_set_at: Instant,
    extranonce_2_size: u8,
}
```

`N = 2` initially. On `visit_new_job`:

- Rotate. Older slot drops.
- Populate from borrowed `MiningJob<'_>`. `lazy_merkle.compute(en2=0,
  &mut slot.merkle_root)` is the last read of borrowed wire data.
- If `clean_jobs`, mark the previous slot `valid = false`.
- Send a snapshot through the `latest_job: Watch<JobSnapshot, 2>` so
  the work generator wakes up.

Per-slot footprint ≈ 150 bytes. No coinbase materials carry across
the borrow boundary.

### Work ring (depth 8)

Outstanding ASIC work — maps a `work_id: u32` (stamped into each
`WorkAssignment` written into `AsicSession`) back to the per-share
context the pool submit needs.

```rust
struct WorkRing<const N: usize> {
    slots: [WorkSlot; N],
    write_index: usize,
}

struct WorkSlot {
    work_id: u32,
    job_id: JobId,
    extranonce_2: heapless::Vec<u8, MAX_EN2>,
    ntime: u32,
    version_bits: u32,
}
```

`WorkAssignment` itself does **not** carry `extranonce_2`. The ASIC
sees only the precomputed header inputs (prev_hash, merkle_root with
en2 already absorbed, target, ntime, etc.) and a `work_id`. The host
recovers `extranonce_2` from the WorkRing on solution lookup.

### Submission gating

When `stratum_task`'s `select!` pulls a `Solution` from
`AsicSession.solutions`:

```rust
let work = work_ring.lookup(solution.work_id)?;       // None → discard
if !job_ring.is_valid(work.job_id) { return; }        // stale → discard
let share = Share {
    id: work.job_id,
    nonce: solution.nonce,
    ntime: solution.ntime,
    version_bits: work.version_bits,
    extranonce_2: &work.extranonce_2,
};
client.submit(&share)?;
```

Stale shares get filtered before they hit the wire — today they go to
the pool and get rejected with "stale" responses.

### Work generator (1 Hz)

```rust
Ticker::every(1s):
    let job = latest_job.try_get()?;
    let en2 = roll_extranonce_2();              // zeros today
    let ntime = job.ntime_base + (Instant::now() - job.ntime_set_at);
    let work_id = next_work_id.wrapping_add(1);
    work_ring.put(work_id, job.id, en2, ntime, version_bits);
    asic.write_work(WorkAssignment {
        work_id,
        prev_hash: job.prev_hash,
        merkle_root: job.merkle_root,           // precomputed; en2=0 today
        target: job.target,
        version: job.version,
        version_mask: job.version_mask,
        nbits: job.nbits,
        ntime,
    });
```

When the ASIC driver lands and we want real extranonce_2 rolling, the
work generator gets a per-job merkle materials view — either by
extending `JobSlot` with coinbase parts (+~2 KB per slot) or by adding
a side-table indexed by `JobId`. Either is local to `stratum-client`;
the trait stays the same.

## Pool configuration

```rust
pub struct PoolConfig {
    pub host_ip: [u8; 4],
    pub port: u16,
    pub reconnect_delay: Duration,
    pub protocol: Protocol,
}

pub enum Protocol {
    StratumV1(Sv1Config),
    StratumV2(Sv2Config),
}

pub struct Sv1Config {
    pub user: &'static str,
    pub password: &'static str,
}

pub struct Sv2Config {
    pub user: &'static str,
    pub pool_pubkey: Option<[u8; 32]>,
}
```

Transport-level fields stay on the outer struct. Protocol-specific
config lives in its variant. `user` is duplicated across both
sub-configs by design — it is protocol-level, not transport-level,
and each adapter only sees the variant it cares about.

Adapters take their typed sub-config:

```rust
impl StratumV1Client {
    pub fn new(net: &PoolConfig, sv1: &Sv1Config) -> Self;
}
impl StratumV2Client {
    pub fn new(net: &PoolConfig, sv2: &Sv2Config) -> Self;
}
```

Firmware destructures at spawn time:

```rust
match &POOL.protocol {
    Protocol::StratumV1(sv1) => spawn(stratum_v1_task(stack, &POOL, sv1)),
    Protocol::StratumV2(sv2) => spawn(stratum_v2_task(stack, &POOL, sv2)),
}
```

## Consuming micro-sv2 in the V2 adapter

`micro-sv2` provides exactly the abstractions the V2 adapter needs.
The umbrella crate `ii-sv2` re-exports them as namespaced modules,
so the V2 adapter takes one path dependency:

```toml
[dependencies]
ii-sv2 = { path = "../../../../micro-sv2/ii-sv2" }
```

Mapping our trait API onto micro-sv2:

| micro-sv2 piece | Role in V2 adapter |
|---|---|
| `ii_sv2::security::initiate(io, rng, authenticator)` | Called from `MiningClient::connect`. `io` is the Embassy `TcpSocket` (already implements `embedded_io_async::Read + Write`). |
| `EstablishedSession::transport` | Held inside the adapter. `EncryptedCodec::from_handshake` wraps it. |
| `ii_sv2::framing::FrameCodec` (impl: `EncryptedCodec`) | Drives `drive()`'s read loop. Header and payload both decrypt in place. Matches the wire-buffer pattern in this doc. |
| `ii_sv2::framing::frame_to_message::<M>` | Typed decode for the "expected response" path during initial setup (e.g. `SetupConnectionSuccess`). |
| `ii_sv2::framing::MessageGroup::dispatch` (`MiningMessages`) | The `accept()` body. Borrows into the same wire buffer; the visitor sees borrowed `MiningJob<'_>` that points into it. |
| `ii_sv2::security::Authenticator` (`ValidateNothing`, `ValidateSignature`, `ValidateTime`, `ValidateFull`) | Constructed by the V2 adapter from `Sv2Config::pool_pubkey`. |
| `ii_sv2::security::Clock` | Implemented in `eth-examples` against NTP-synced wall time. Required only for `ValidateTime` / `ValidateFull`. |

The trait crate (`mining-protocol`) stays protocol-agnostic. Nothing
above leaks into it. The V2 adapter is the only crate in
`eth-examples` that depends on `ii-sv2`.

### V2 transport bound

Where `mining-protocol` requires `T: Read + Write` for the transport,
this is `embedded_io_async::Read + embedded_io_async::Write` — the
same traits `micro-sv2`'s handshake uses. Embassy `TcpSocket` already
implements both; tokio `tokio::io::duplex` works via
`embedded-io-adapters` for host tests in `eth-examples-host`.

### Translating Sv2Config into an Authenticator

`Sv2Config::pool_pubkey: Option<[u8; 32]>` maps to:

- `None` → `ValidateNothing`
- `Some(key)` → `ValidateSignature { authority: PublicKey::new(key), ctx: &SECP_CTX }`

`SECP_CTX` is a `&'static Secp256k1<VerifyOnly>` constructed at
startup from a static pre-allocation buffer via
`Secp256k1::preallocated_verification_only`. Lifting `Authenticator`
itself to a config type parameter (`Sv2Config<A>`) is a non-breaking
move when we want it later.

### Upstream gaps that affect the V2 adapter

Two `micro-sv2` user stories are still deferred and matter for our
adapter:

- **`MINE004` — share submission.** `SubmitSharesStandard`,
  `SubmitSharesSuccess`, `SubmitSharesError` are not yet in
  `ii-sv2::messages`. `MiningClient::submit` for V2 cannot be
  finished until these land. Two paths: wait on upstream, or
  contribute the messages ourselves (small, well-specified —
  `channel_id u32, seq_num u32, job_id u32, nonce u32, ntime u32,
  version u32` and the response forms with their error codes).
- **`CHAN003` — channel housekeeping.** `Reconnect`, `UpdateChannel`,
  `CloseChannel` are deferred upstream. The V2 adapter never emits
  `SessionUpdate::Reconnect` until these land; pool-side reconnects
  are handled at the TCP layer in the meantime (socket close →
  `stratum-client`'s outer reconnect loop, same as today's V1
  behavior in `ubos-backend`).

Phase 11 below should call out which of these we'll wait on vs
contribute.

## Dev environment

There is no QEMU setup in this repo. Three test paths:

| Layer | How |
|---|---|
| Pure protocol — parser, encoder, framer, Noise codec | Host unit tests in `src/tests/`, same model `stratum-v1` already uses. |
| State machine — the adapter's `MiningClient` impl with a fake `embedded-io-async` transport | Host unit tests with an in-memory pipe. No real TCP. |
| Embassy integration — full task + `embassy-net` + Noise + pool | A new `eth-examples-host` sibling crate using `embassy-net` over Linux TUN/TAP, the upstream `embassy/examples/std` pattern. |

Real-hardware flashing only at the end. A std-only smoke binary
analogous to `stratum-v1/examples/probe.rs` connects to a real
pool and exercises the full path without `embassy-net`.

## Deferred decisions

| Item | Why deferred |
|---|---|
| `Authenticator` trait (replacing `Option<[u8; 32]>` for V2 pool key) | Generalize once we need a second variant beyond `ValidateNothing` / `ValidateSignature`. |
| `ValidateTime` / `ValidateFull` | Need a `Clock` abstraction; build it when NTP-synced wall time is available to the V2 adapter. |
| Rolling extranonce_2 in CPU solver | Trait already supports it. CPU solver doesn't need it on these MCUs. Land with the ASIC driver. |
| Ring depth `N > 2` | `N = 2` is enough for non-ASIC throughput. Promote to `const` parameter when an ASIC driver demonstrates it matters. |
| Owned per-slot rolling materials (~2KB extra per slot) | Land with the ASIC driver. |
| `JobId.0 == V2 wire id` short-circuit | Cleaner to keep both adapters on the same translation-map shape. Revisit if memory pressure shows up. |
| Stratum prometheus metrics | See [Pending: stratum prometheus metrics](#pending-stratum-prometheus-metrics) below. |

## Pending: stratum prometheus metrics

### Goal

Surface lifetime stratum-session counters on the existing prometheus
exposition so we can answer "is the board actually submitting and
getting paid?" without poking at defmt logs.

### Counters

| Metric (prometheus name) | Source | Bumped at |
|---|---|---|
| `stratum_submits_sent_total` | atomic u64 in `stratum-client::metrics` | after `client.submit(&share)` returns `Ok` (i.e. queued for the next `drive` flush) |
| `stratum_submits_accepted_total` | atomic u64 | `visit_submit_result(result)` with `result.accepted == true` |
| `stratum_submits_rejected_total` | atomic u64 | `visit_submit_result(result)` with `result.accepted == false` |
| `stratum_accepted_difficulty_sum` | atomic u64 | accepted path: `+ approx_u32(JobSlot.target)` looked up via `state.job_ring.lookup(result.id)` |

Pending count derives in prometheus as `sent - accepted - rejected`.

`approx_u32` is a new pure helper in
`mining_protocol::difficulty` (numeric counterpart of the existing
`approx_string`).

### Design refinements (post-investigation)

- **No per-state FIFO is required.** Since V1's `SubmitResult.id`
  now carries the real `JobId` (after the pending-id-map work in
  `c0331b1`), `visit_submit_result` can look up the originating
  `JobSlot` and read its `target` directly. The earlier sketch
  needed a FIFO only because `JobSlot.target` was being rewritten
  mid-job by `SetTarget` updates (see prerequisite below) — that's
  a real bug in current code, not the protocol's intended
  behaviour.
- **`JobRing::lookup(JobId) -> Option<&JobSlot>`** — small API
  addition next to the existing `is_valid`. Cheap; depth is 2.
- **Exposition:** `eth-examples` gains a regular workspace dep on
  `stratum-client`. `eth-examples::metrics::render_exposition`
  reads the four atomics and emits prometheus counter lines next
  to the existing `board_uptime_seconds` block.

### Prerequisite: drop mid-job retarget in `StratumState`

`stratum-client::state.rs`'s visitor currently overwrites the
**current** `JobSlot.target` (and `version_rolling_mask`) when
`SetTarget` / `SetVersionMask` arrives mid-job:

```rust
SessionUpdate::SetTarget(target) => {
    self.target = *target;
    if let Some(latest) = self.job_ring.current_mut() {
        latest.target = *target;     // <-- BUG: retroactive retarget
    }
}
```

Per the stratum spec, `set_difficulty` / `SetTarget` configures the
difficulty for the **next** job, not the in-flight one. The current
slot's target must stay frozen at whatever was set when
`visit_new_job` captured `state.target` into the slot. Same rule
for `version_rolling_mask`.

The mid-job rewrite is the only thing that made the FIFO sketch
necessary; fix it first and the metrics design simplifies.

### Checklist

- [ ] **Prerequisite:** remove the `current_mut()` retarget for
      `SessionUpdate::SetTarget` and `SessionUpdate::SetVersionMask`
      in `stratum-client::state.rs`. Just update `self.target` /
      `self.version_mask`; let `visit_new_job` pick them up for the
      next slot.
- [ ] Audit V1's `SetExtranonce` notification handler in
      `stratum-v1::client.rs` for the same kind of mid-stream
      rewrite; document or fix to match the agreed semantics.
- [ ] Add `mining_protocol::difficulty::approx_u32(target_le) ->
      Option<u32>` and refactor `approx_string` to call it.
- [ ] Add `JobRing::lookup(JobId) -> Option<&JobSlot>`.
- [ ] Add `stratum-client::metrics` submodule: four `AtomicU64`
      statics + `submits_sent()` / `submits_accepted()` /
      `submits_rejected()` / `accepted_difficulty_sum()` getters +
      crate-private `record_*` helpers.
- [ ] Wire bookkeeping: `run::handle_solution` bumps
      `SUBMITS_SENT` after `client.submit(&share)?` returns Ok.
      `StratumState::visit_submit_result` bumps accept/reject and,
      on accept, adds `approx_u32(JobSlot.target)` looked up via
      `state.job_ring.lookup(result.id)` (defaults to 0 contribution
      when the slot was rotated out by `clean_jobs` between submit
      and response — accept count still goes up).
- [ ] Add `stratum-client = { workspace = true }` to
      `eth-examples/Cargo.toml`.
- [ ] Render the four counters in
      `eth-examples::metrics::render_exposition` (next to
      `board_uptime_seconds`). Names: `stratum_submits_sent_total`,
      `stratum_submits_accepted_total`,
      `stratum_submits_rejected_total`,
      `stratum_accepted_difficulty_sum`.
- [ ] Verify on rp2350 against a real pool: submit a handful of
      shares, scrape `/metrics`, confirm sent/accepted/rejected
      add up and difficulty_sum is non-zero.

### Open questions (revisit before implementing)

- **Slot-rotated-out fallback.** When a share's response arrives
  after `clean_jobs` invalidated the originating slot, we can't
  recover its target. Options: credit 0 (accept rate stays
  correct, difficulty_sum slightly under-counts), credit
  `state.target` (less accurate but non-zero), or skip the share
  entirely (also skip the accept counter — wrong). Default to
  "credit 0" unless we measure that rotation-during-flight is
  common.
- **Per-session vs lifetime counters.** Currently sketched as
  process-lifetime atomics. If we want per-session counts that
  reset on reconnect, the rendering layer can compute them from
  snapshots; no new bookkeeping needed.
- **Rate-style metrics (1-min hashrate-from-pool, rolling
  acceptance ratio).** Out of scope for the first pass; prometheus
  can derive them from the counters.

## Implementation phases

Each phase is independently `cargo check`-clean, `cargo clippy
-- -D warnings`-clean, and reviewable in isolation.

1. **Crate scaffolding.** Add `mining-protocol`, `stratum-v2`,
   `stratum-client`, `asic-cpu-mock`, and `eth-examples-host` to
   the workspace as empty stubs. Rename `stratum-v1-core` to
   `stratum-v1`. Wire flake `checks` derivations for each new crate.
2. **`mining-protocol` types.** Define `JobId`, `MiningJob`, `Share`,
   `SessionUpdate`, `SubmitResult`, `LazyMerkle`, `Error`. No traits
   yet. Tests for `LazyMerkle::compute` against known V1 vectors.
3. **`mining-protocol` traits.** Define `MiningClient`,
   `MiningClientVisitor`. No impls.
4. **`stratum-v1` internal restructure.** Move existing
   `messages.rs`, `parse.rs`, `line_buffer.rs` under `src/wire/`.
   Move `decode_hex` from `ubos-backend` into `src/hex.rs`. No new
   public API. `cargo check` and existing tests stay green. The V2
   alignment work (Encode trait, V1Message flat enum) is deferred
   to a post-adapter cleanup pass — landing it now would force
   mechanical edits across `ubos-backend` for a crate that is
   slated for deletion in phase 9.
5. **`stratum-v1` MiningClient impl.** Add `client.rs`,
   `state.rs`, `scratch.rs`. Implement `MiningClient` for
   `StratumV1Client`. Two-buffer scheme, JobId translation map.
   Host unit tests with in-memory pipe. New `examples/client_probe.rs`
   exercises end-to-end in std.
6. **New `stratum-client` crate.** Replaces `ubos-backend` for the
   migrated firmwares. Generic over `MiningClient`. Owns `JobRing<2>`,
   `WorkRing<8>`, `AsicSession`. Tasks: `stratum_task`,
   `work_generator_task`. Implements `MiningClientVisitor`. Old
   `ubos-backend` stays in tree until all firmwares migrate.
7. **New `asic-cpu-mock` crate.** Implements the ASIC side of
   `AsicSession` (read `work` Watch, hash, push to `solutions`
   Channel). Replaces today's `cpu-miner` integration for migrated
   firmwares.
8. **rp2040 firmware migration.** Wire rp2040 to `stratum-client` +
   `asic-cpu-mock`. Verify against ckpool. Other firmwares stay
   on `ubos-backend` for now.
9. **Migrate remaining firmwares.** rp2350, stm32f407, stm32f439,
   bmm-pico move to `stratum-client`. Pick `asic-cpu-mock` or the
   `uart-asic` AsicSession adapter per board. Once all firmwares
   are migrated, `ubos-backend` and `cpu-miner` are deleted.
10. **Host integration harness.** Stand up `eth-examples-host` with
    `embassy-net` + tuntap. Smoke test against ckpool / regtest pool.
11. **V2 adapter (`stratum-v2`) — read-side.** Implement
    `MiningClient::connect` (Noise handshake → SetupConnection →
    OpenStandardMiningChannel) and the `drive` / `accept` read path
    for all currently-implemented `MiningMessages` variants
    (NewMiningJob, SetNewPrevHash, SetTarget, *Success, *Error).
    `MiningClient::submit` returns `Err(NotImplemented)` for now.
    Host unit tests + integration via the harness from phase 10.
12. **`SubmitSharesStandard` upstream contribution.** Add
    `SubmitSharesStandard`, `SubmitSharesSuccess`,
    `SubmitSharesError` to `micro-sv2::messages` (and the
    `MiningMessages` dispatcher). Either as a PR upstream or as a
    local fork patch — decide at the time based on micro-sv2's
    velocity.
13. **V2 adapter — submit path.** Once phase 12 lands,
    `MiningClient::submit` encodes `SubmitSharesStandard` with the
    channel-bound `seq_num` counter and feeds responses through
    `visit_submit_result`.
14. **V2 firmware enablement.** Pick one board (probably rp2350 —
    most headroom for the secp256k1 verify-only context). Wire
    `Protocol::StratumV2` and verify against a SV2 pool.

Phases 1–8 land V1 on the new architecture (rp2040 only) without
behavior change beyond the new pipeline. Phase 9 finishes the V1
migration. Phase 10 unblocks fast V2 iteration. Phases 11–14 add
V2 incrementally; the read-side (phase 11) can be exercised against
a pool well before the submit path is complete.

## Risks and unknowns

- **Cortex-M0+ binary size on RP2040.** Adding a trait layer plus a
  copy step in `visit_new_job` should be small, but worth measuring at
  phase 5. `cargo size --release -- -A` budget is in `README.md`.
- **`large_futures` clippy** already complains about
  `ubos-backend::stratum_task`. The refactor must not regress that.
  The borrowed-not-owned visitor types help — `LineAction` (~1.6 KiB)
  is no longer the dispatch shape.
- **secp256k1 verify-only context size on RP2040.** The
  `preallocated_verification_only` context is on the order of tens of
  KB of static RAM. Comfortable on RP2350 / STM32F439 / STM32F407;
  visible on RP2040 (264 KB total). Measure at phase 12; if too tight,
  fall back to `ValidateNothing` on RP2040 or use the secp256k1
  `low-memory` feature.
- **Cancellation safety of `drive()` for V2.** `embedded_io_async::Read::read_exact`
  is not cancel-safe — partial reads leave AEAD chain state ready to
  consume the rest. Structure `drive()` so each call reads at most one
  full frame (header → decrypt-on-accept). Outer `select!` between
  `drive()` and `asic.solutions.recv()` is then safe because each
  drive call is atomic. This matches today's `LineBuffer` discipline
  but is more load-bearing because Noise nonces don't tolerate replay.
- **micro-sv2 upstream completeness.** Read-side messages are fully
  in place; share submission and channel housekeeping (`MINE004`,
  `CHAN003`) are deferred. Phases 10–11 either wait or contribute
  upstream. Either path, the V2 adapter's read-side (phase 9) is
  unblocked and can be exercised independently.
