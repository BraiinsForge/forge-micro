# Architecture: stratum-client + ASIC mock

Visual reference for the stratum stack. This file is the picture;
`plan/STRATUM_IMPLEMENTATION_PLAN.md` is the historical rationale. Kept
aligned with the code in `ubos/stratum-client` — when the two
disagree, the code wins and this file needs a patch.

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
   │   │ run_stratum_client — one loop per pool session, three   │       │
   │   │ wake sources (asic = PoolHandle for this pool's slot)   │       │
   │   │   loop {                                                │       │
   │   │     select3(                                            │       │
   │   │       client.drive(t)       → client.accept(state)      │       │
   │   │       asic.take_solution()  → handle_solution(state)    │       │
   │   │       work_ticker → state.publish_work(asic, step)      │       │
   │   │     )                                                   │       │
   │   │   }                                                     │       │
   │   └─────────────────────────────────────────────────────────┘       │
   │                                                                     │
   │   visit_new_job (Visitor):        state.publish_work (work tick):   │
   │     slot = job_ring.next_slot()     slot = job_ring.current()?      │
   │     merkle.compute(en2 = zeros)     slot.ntime_offset += 1          │
   │       → slot.merkle_root              (capped NTIME_ROLL_MAX_SECS)  │
   │     slot.prev_hash = *job.ph        work_id = next_work_id++        │
   │     slot.target    = *job.target    work_ring.put(OutstandingWork{  │
   │     slot.{version, nbits} = ...       work_id, slot.job_id,         │
   │     slot.ntime_base = job.ntime       extranonce_2 = zeros })       │
   │     slot.ntime_offset = 0           asic.publish_work(              │
   │     slot.valid = true                 WorkAssignment{ work_id,      │
   │     if clean_jobs:                      prev_hash, merkle_root,     │
   │       job_ring.invalidate_previous()    target, version, nbits,     │
   │                                         ntime = base + offset })    │
   │   handle_solution (chip found something):                           │
   │     work = work_ring.lookup(sol.work_id)?   else drop (rotated)     │
   │     job_ring.is_valid(work.job_id)?         else drop (clean_jobs)  │
   │     client.submit(&Share{ id: work.job_id,                          │
   │       nonce: sol.nonce, ntime: sol.ntime,                           │
   │       version_bits: sol.version_bits,                               │
   │       extranonce_2: &work.extranonce_2 })                           │
   │                                                                     │
   │   visit_session_update: log only.  visit_submit_result: counters.   │
   │                                                                     │
   │   ┌──────────────────────┐    ┌──────────────────────────────┐      │
   │   │ JobRing<2> rotating  │    │ WorkRing<8>  FIFO            │      │
   │   │ pool's last 2 jobs   │    │ outstanding ASIC work        │      │
   │   │ JobSlot:             │    │   work_id u32 → {            │      │
   │   │   job_id, valid      │    │     job_id: JobId,           │      │
   │   │   prev_hash[32]      │    │     extranonce_2: hl::Vec }  │      │
   │   │   target[32]         │    │                              │      │
   │   │   merkle_root[32]    │    │ ntime / version_bits are NOT │      │
   │   │   version, nbits     │    │ stored here: the Solution    │      │
   │   │   ntime_base         │    │ echoes whichever values the  │      │
   │   │   ntime_offset       │    │ ASIC actually hashed with    │      │
   │   │   en2_size, clean    │    └──────────────────────────────┘      │
   │   └──────────────────────┘                                          │
   └─────────────────────┬────────────────────────────────────────┬──────┘
                         │                                        ▲
                         │ StratumState::publish_work(asic, step) │
                         │   → work[slot] (unconditional)         │
                         ▼                                        │
   ┌─────────────────────────────────────────────────────────────────────┐
   │        AsicSession<N>   (one channel pair per pool slot)            │
   │      [in stratum-client; passed as &'static to both sides;          │
   │       each session sees only its slot through a PoolHandle]         │
   │                                                                     │
   │   work:      [Watch<CSRawMutex, WorkAssignment, 2>; N]              │
   │              session slot writes latest  →  dispatcher reads the    │
   │              scheduler-chosen slot's cell per work tick (the pool   │
   │              switch; sessions stay policy-free)                     │
   │                                                                     │
   │   solutions: [Channel<CSRawMutex, Solution, 8>; N]                  │
   │              ASIC routes each solution to the slot that dispatched  │
   │              the originating work  →  that session submits it       │
   └─────────────────────────────────────────────────────────────────────┘
                         │                                        ▲
                         │ work_receivers[scheduler.evaluate()]   │
                         │   .try_get() per work tick             │
                         │   (nothing dispatchable → skip tick)   │
                         ▼                                        │
   ┌─────────────────────────────────────────────────────────────────────┐
   │           BM1362 hashboard driver (ubos-mining) or inline CPU mock   │
   │   [ubos-mining crate on hardware; host-runner::cpu_mock as fallback] │
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

## Why two rings: two coordinate systems

`StratumState` is a translation table between two coordinate systems.
The pool addresses work as *(job_id, extranonce_2, ntime,
version_bits)*; the chip addresses it as *(work_id, nonce)*. The two
worlds also have different cardinalities and lifetimes — one
long-lived job fans out into many short-lived work assignments, one
per second, differing only in `ntime` (extranonce_2 is fixed at zeros
and the merkle root is folded once at job arrival, so the ntime roll
is the only thing making each second's assignment fresh work). That
cadence is the board's choice, `NtimeRoll`: a board whose chip searches
the nonce alone passes `Host` and gets the roll above; a board whose
chip searches ntime on its own passes `Chip`, and then each job is
published once on arrival, `ntime_offset` stays zero, and the ntime a
solution carries is the one the chip used.

Downstream, pool → chip:

```
ASIC                              HOST                                 STRATUM
════                              ════                                 ═══════
                       job_ring (few, long-lived)
                      ┌──────────────────────────┐  visit_new_job()
                      │ JobSlot: pool's job in   │◄———— MiningJob ——— notify /
                      │ canonical binary form    │   (borrowed; the   NewMiningJob
                      └────────────┬─────────────┘    slot owns a copy)
                                   │ tick: ntime_offset += 1
                                   ▼
              WorkAssignment   state.publish_work()
 chip ◄══════ {work_id, header ┌──────────────────────────┐
 (Watch,       fields, ntime = │ work_ring.put(receipt:   │
  latest-wins) base + offset}  │  work_id → job_id + en2) │
                               └──────────────────────────┘
                                work_ring (many, short-lived)
```

Upstream, chip → pool — the receipt written at publish time is the
only memory of which job and which extranonce_2 produced a given
`work_id`, and the two rings act as two staleness gates:

```
ASIC                              HOST                                 STRATUM
════                              ════                                 ═══════
 chip ══════► Solution{work_id,  handle_solution()
 (Channel,     nonce, ntime,    ┌────────────────────────────────┐
  bounded)     version_bits}    │ ① work_ring.lookup(work_id)    │
                                │     miss → DROP (ring rotated  │
                                │     past it; ≈ >8 s in flight) │
                                │ ② job_ring.is_valid(job_id)    │
                                │     no → DROP (clean_jobs      │
                                │     invalidated the job)       │
                                │ ③ translate back to pool       │  submit()
                                │    coords: Share{job_id, en2,  │—————————————► pool
                                │    nonce, ntime, version_bits} │
                                └────────────────────────────────┘
```

The ring depths encode the intended history windows:

- `JobRing<2>` rotates: current + the job that just rotated out.
  Pools generally accept shares against the previous job unless
  `clean_jobs` says otherwise, so two slots is the useful history.
- `WorkRing<8>` is a FIFO bounding how long a solution may sit in the
  chip FIFO plus the solution channel before its submit context is
  forgotten, with no heap. What eight slots buy follows the publish
  cadence: `NtimeRoll::Host` republishes every second, so they are
  about eight seconds; `NtimeRoll::Chip` publishes once per pool job,
  so they are the last eight pool jobs and not a span of time.

The `AsicSession` seam mirrors the same asymmetry. Work rides a
`Watch` (latest-wins cell, never a backlog: a slow chip skips
straight to the newest assignment); solutions ride a bounded
`Channel` (a real queue: each one is potential payment and must not
be overwritten). `CriticalSectionRawMutex` makes both ends safe to
share across cores, which is how dual-core boards run the serial
task on core 1.

Multipool multiplies the seam, not the loop: `AsicSession<N>` holds
one work/solution pair per pool slot, every configured pool runs its
own unchanged `run_stratum_client` session against its `PoolHandle`,
and the runtime pool switch lives entirely on the consumer side —
`dispatch_work` asks its `Scheduler` at each work tick which slot to
serve and pulls from that slot's cell. The `RatioScheduler` splits
delivered work between the slots at the per-pool `weight` ratio set
in the config, fed back by difficulty-1 chip shares; weight-0 slots
are failover backups (see `docs/MULTIPOOL_DESIGN.md`, work-scheduler
and config-driven weights sections). The
serial worker's registry records which slot each dispatch came from,
so a solution returns to the session that owns the work even if the
scheduler moved on while the chip was hashing; that session still
submits its in-flight shares over its open connection. A session
clears its cell on teardown, and the dispatcher routes around empty
cells, so a dead pool's last job is never re-fed to the chip.

## Cycle frequencies

| Loop | Period | Driver |
|------|--------|--------|
| Pool I/O | event-driven | network bytes arriving / Solution arriving in `select3` |
| Stratum visitor dispatch | per frame | inside `accept()` |
| Work publish | 1 Hz with `NtimeRoll::Host`, once per job with `Chip` | `work_ticker` arm of the same `select3` loop |
| ASIC poll for work change | ~100 ms | yield boundary in CPU mock; polling cadence in real driver |
| ASIC produces solutions | as found | non-blocking `try_send` into the channel |
| Pool switch takes effect | next work tick | `dispatch_work` asks the scheduler before each dispatch |

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
WorkRing slot carries the per-share submit context (job_id,
extranonce_2) so a returning Solution can reconstruct the full wire
submit via its `work_id`, with `ntime` and `version_bits` echoed by
the ASIC in the Solution itself.
