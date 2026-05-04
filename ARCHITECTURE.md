# Architecture: stratum-client + ASIC mock

Visual reference for the design captured in `IMPLEMENTATION_PLAN.md`.
This file is the picture; the plan is the rationale.

## System overview

```
═════════════════════════════════════════════════════════════════════════════
                  POOL  (V1 JSON-RPC  or  V2 binary+noise)
═════════════════════════════════════════════════════════════════════════════
                            ▲                    │
              encoded submit│                    │ encrypted/plain frames
                            │                    ▼
                     ┌─────────────────────────────────┐
                     │   embassy-net::TcpSocket        │
                     │   impls embedded_io_async::*    │  ← firmware-owned
                     └─────────────────────────────────┘
                            ▲                    │
                            │                    ▼
   ┌─────────────────────────────────────────────────────────────────────┐
   │           MiningClient adapter   (V1 OR V2)                         │
   │           [stratum-v1 OR stratum-v2 — only place protocol lives]    │
   │                                                                     │
   │   wire_buffer: [u8; N]    ← raw frame; AEAD-decrypted in place (V2) │
   │   decode_scratch          ← V1 only: hex-decoded prev_hash etc.     │
   │   JobId map: [(JobId, wire_id); 2]   stamped on visit_new_job       │
   │                                                                     │
   │   V1 specifics:           V2 specifics:                             │
   │     LineBuffer              Noise TransportState (nonces, keys)     │
   │     JSON-RPC seq_id         EncryptedCodec, channel_id, seq_num     │
   │     hex / per-u32 swap      secp256k1 verify ctx (Authenticator)    │
   │                                                                     │
   │   connect(t)  :  V2 noise handshake; V1 no-op                       │
   │   drive(t)    :  read bytes → wire_buffer, decrypt header (stash)   │
   │   accept(v)   :  AEAD-open payload, parse, translate to MiningJob,  │
   │                  call visitor    ◄── borrow scope ENDS at return    │
   │   submit(s)   :  encode Share → wire bytes → t.write_all            │
   └─────────────────────────────────────────────────────────────────────┘
        ▲                                 │
        │ submit(&Share)                  │ visit_new_job(MiningJob<'_>)
        │                                 │ visit_session_update(...)
        │                                 │ visit_submit_result(...)
        │                                 ▼
   ┌─────────────────────────────────────────────────────────────────────┐
   │                       stratum-client                                │
   │      [crate: stratum-client — protocol-agnostic from here on]       │
   │                                                                     │
   │   ┌─────────────────────────────────────────────────────────┐       │
   │   │ stratum_task                                            │       │
   │   │   loop {                                                │       │
   │   │     select!(                                            │       │
   │   │       client.drive(t).await   →  client.accept(self)    │       │
   │   │       asic.solutions.recv()   →  submit_handler(self)   │       │
   │   │     )                                                   │       │
   │   │   }                                                     │       │
   │   └────────┬───────────────────────────┬────────────────────┘       │
   │            │                           │                            │
   │   impl Visitor for self          submit_handler                     │
   │   visit_new_job:                 ──────────────                     │
   │     slot = job_ring.next_slot()  sol = AsicSession.recv()           │
   │     lazy_merkle.compute(en2=0,   work = work_ring.lookup(           │
   │          &mut slot.merkle_root)         sol.work_id) ?              │
   │     slot.prev_hash = *job.ph     if !job_ring.is_valid(             │
   │     slot.target = *job.target          work.job_id):                │
   │     slot.{ntime_base, version,     drop_stale; return               │
   │            mask, nbits} = ...    else:                              │
   │     slot.valid = true              share = Share {                  │
   │     if clean_jobs:                   id: work.job_id,               │
   │       job_ring.invalidate_prev()     nonce: sol.nonce,              │
   │     latest_job.send(snapshot)        ntime: sol.ntime,              │
   │                                      extranonce_2: work.en2,        │
   │   visit_session_update:              version_bits: work.vb,         │
   │     update target/mask/...         }                                │
   │                                    client.submit(&share)            │
   │   visit_submit_result:                                              │
   │     log + metrics                                                   │
   │                                                                     │
   │   ┌──────────────────────┐    ┌──────────────────────────────┐      │
   │   │ JobRing<2>           │    │ WorkRing<8>                  │      │
   │   │ pool's last 2 jobs   │    │ outstanding ASIC work        │      │
   │   │  + valid bit         │    │   work_id u32 → {            │      │
   │   │ Slot:                │    │     job_id: JobId,           │      │
   │   │   id: JobId          │    │     extranonce_2: hl::Vec,   │      │
   │   │   valid: bool        │    │     ntime: u32,              │      │
   │   │   prev_hash[32]      │    │     version_bits: u32,       │      │
   │   │   target[32]         │    │   }                          │      │
   │   │   merkle_root[32]    │    └──────────────────▲───────────┘      │
   │   │   {ntime_base,       │                       │                  │
   │   │    version, mask,    │            record_outstanding()          │
   │   │    nbits}            │                       │                  │
   │   │   en2_size: u8       │                       │                  │
   │   └──────┬───────────────┘                       │                  │
   │          │                                       │                  │
   │   latest_job: Watch<JobSnapshot, 2>              │                  │
   │          │                                       │                  │
   │   ┌──────▼───────────────────────────────────────┴───────────┐      │
   │   │ work_generator_task                                      │      │
   │   │   Ticker every 1 s:                                      │      │
   │   │     job = latest_job.try_get() ?                         │      │
   │   │     en2 = roll() (zeros today)                           │      │
   │   │     ntime = job.ntime_base + (now - job.t0)              │      │
   │   │     work_id = self.next_work_id()                        │      │
   │   │     work_ring.put(work_id, job.id, en2, ntime, vb)       │      │
   │   │     asic.write_work(WorkAssignment {                     │      │
   │   │       work_id, prev_hash, merkle_root, target,           │      │
   │   │       version, version_mask, nbits, ntime,               │      │
   │   │     })                                                   │      │
   │   └─────────────────┬────────────────────────────────────────┘      │
   └─────────────────────┼────────────────────────────────────────┬──────┘
                         │                                        ▲
                         │ Watch::sender().send()                 │
                         ▼                                        │
   ┌─────────────────────────────────────────────────────────────────────┐
   │              AsicSession   (the "item" abstraction)                 │
   │      [in stratum-client; passed as &'static to both sides]          │
   │                                                                     │
   │   work:      Watch<CSRawMutex, WorkAssignment, 2>                   │
   │              host (work_generator) writes  →  ASIC polls latest     │
   │                                                                     │
   │   solutions: Channel<CSRawMutex, Solution, 8>                       │
   │              ASIC sends  →  host receives (bounded queue)           │
   └─────────────────────────────────────────────────────────────────────┘
                         │                                        ▲
                         │ work.receiver().changed()              │
                         │   / try_changed()                      │
                         ▼                                        │
   ┌─────────────────────────────────────────────────────────────────────┐
   │                asic-cpu-mock   (or uart-asic adapter)               │
   │      [crate: asic-cpu-mock — replaces today's cpu-miner]            │
   │                                                                     │
   │   Loop:                                                             │
   │     if work_receiver.try_changed() {                                │
   │         current = new_work; nonce_start = 0                         │
   │     }                                                               │
   │     header = build_header(current)   ← memcpy fields, no swaps      │
   │     for nonce in nonce_start..nonce_start + CHUNK {                 │
   │         h = sha256d(header_with_nonce)                              │
   │         if h <= current.target {                                    │
   │             solutions.try_send(Solution {                           │
   │                 work_id: current.work_id,                           │
   │                 nonce,                                              │
   │                 ntime: current.ntime,                               │
   │                 version_bits: 0,                                    │
   │             })                                                      │
   │         }                                                           │
   │     }                                                               │
   │     yield (); poll work cell at chunk boundary (~100 ms wall clock) │
   └─────────────────────────────────────────────────────────────────────┘
```

## Cycle frequencies

| Loop | Period | Driver |
|------|--------|--------|
| Pool I/O | event-driven | network bytes arriving / Solution arriving in `select!` |
| Stratum visitor dispatch | per frame | inside `accept()` |
| Work generator | 1 Hz | `Ticker::every(1s)` |
| ASIC poll for work change | ~100 ms | yield boundary in CPU mock; polling cadence in real driver |
| ASIC produces solutions | as found | non-blocking `try_send` into the channel |

## Where the protocols enter

```
                       trait MiningClient
                       (mining-protocol crate)
                          /          \
                         /            \
              ┌────────────────┐    ┌────────────────┐
              │   stratum-v1   │    │   stratum-v2   │
              │                │    │                │
              │  JSON-RPC      │    │  ii-sv2        │
              │  LineBuffer    │    │  Noise NX      │
              │  hex codec     │    │  EncryptedCodec│
              │  set_difficulty│    │  SetTarget     │
              │   ↓ float      │    │   ↓ &[u8;32]   │
              │  target_from_  │    │  passthrough   │
              │     diff       │    │                │
              │  prev_hash:    │    │  prev_hash:    │
              │   per-u32 swap │    │   passthrough  │
              └────────────────┘    └────────────────┘
                         \            /
                          \          /
                           \        /
                    borrowed MiningJob<'_>
                    (canonical block-header byte order,
                     binary not hex, JobId not strings)
                            │
                            ▼
                     stratum-client visitor
                  (protocol-agnostic from here on)
```

The two adapters are the only code in the system that knows about
JSON-RPC vs binary, hex vs binary, V1 byte-swap, noise crypto,
channel_id, etc. From the visitor downward (stratum-client →
AsicSession → ASIC mock) the only protocol-aware thing is
`MiningClient::submit(&Share)`, which routes back into the adapter
for encoding.

## Lifetime and ownership boundary

```
  wire_buffer borrow lifetime
  ───────────────────────────
  [drive() returns]
  [accept() begins]
      FrameView<'a>     ─┐
      MiningJob<'a>      │   all borrowed slices point
        prev_hash &      │   into wire_buffer (V2) or
        LazyMerkle &     │   decode_scratch (V1)
        target &         │
      visit_new_job(     │
        lazy_merkle.compute(en2 = 0, &mut slot.merkle_root)  ◄── LAST READ
        slot.prev_hash = *job.prev_hash      [memcpy 32]
        slot.target    = *job.target         [memcpy 32]
        slot.{u32 fields} = ...
      )                 ─┘
  [accept() returns]
  ───────────────────────────
  wire_buffer free for next drive()
```

Owned ring slot ≈ 150 bytes. Two slots in the JobRing = ~300 bytes.
No coinbase-sized data lives across the borrow boundary. The
merkle root is computed once, before the borrow ends; the
WorkRing slot carries the small per-share rolling state
(extranonce_2, ntime, version_bits) so a returning Solution can
reconstruct the full submit context via its `work_id`.
