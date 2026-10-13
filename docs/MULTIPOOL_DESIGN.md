# Multipool: parallel stratum sessions with scheduler-driven dispatch

> **Status.** Implemented: the multipool mechanism under UBOS-49
> ("Add second stratum channel"), the work scheduler and its
> config-driven weights under UBOS-72. Step tracking in
> `/IMPLEMENTATION_PLAN.md`. Kept aligned with the code in
> `ubos/stratum-client`, `ubos/ubos-mining`, and `ubos/ubos-firmware`
> — when the two disagree, the code wins and this file needs a patch.

## Goal

Run one stratum session per configured pool entry (up to
`MAX_POOLS = 3`) in parallel, all connected at the same time, with
exactly one slot feeding work to the ASIC at any work tick. Selection
happens on the consumer side: sessions always publish their latest
job; the serial worker's scheduler (see the work-scheduler section
below) decides per tick which slot's job to dispatch.

## Non-goals

- A hidden built-in dev-fee pool entry. Sessions map 1:1 to the
  user-visible `Config::pool` list. A hidden slot can be added later
  by instantiating the session type with a larger `N`.
- Per-chip work splitting (some chips on pool A, others on pool B).
  That requires per-chip work routing inside `ubos-mining` dispatch
  and is a separate, deeper change. Time-slice switching achieves a
  fee percentage with the single existing chip pipeline and does not
  preclude it.

## Current state (what this changes)

- `AsicSession` = one `Watch<WorkAssignment>` (latest-wins work cell)
  + one `Channel<Solution>`. One `pub static ASIC_SESSION` in
  `ubos-firmware`, referenced by both the stratum session loop and
  each board's `serial_task`.
- `ubos_firmware::run_stratum_session` uses `snapshot.pool.first()`
  only.
- `run.rs` already anticipates multipool: `POOL_ALIVE_BITMAP` is one
  bit per pool index, and `record_reconnect_attempt` /
  `PRIMARY_POOL_INDEX` carry comments saying to add a `pool_index`
  parameter when a backup-pool task appears.
- `ubos-config`'s `stratum_restart()` is an embassy `Signal`, which
  holds a single waker. Three pool tasks waiting on it would starve
  each other; it must become a broadcast.

## Design

### `AsicSession<const N: usize>`: per-slot channels, both directions

The session becomes const-generic over the slot count, with one work
`Watch` and one solution `Channel` per slot:

```
pub struct AsicSession<const N: usize> {
    work: [Watch<CriticalSectionRawMutex, WorkAssignment, WATCH_RECEIVERS>; N],
    solutions: [Channel<..., Solution, SOLUTION_QUEUE_DEPTH>; N],
}
```

- The session holds no selection state: which slot feeds the chip is
  the dispatcher's decision, made per work tick by its scheduler
  (see the work-scheduler section). No signalling toward the
  sessions; they are policy-free and never know whether they are
  being mined.
- Host side, per slot: `pool_handle(slot) -> PoolHandle` — a `Copy`
  value `{ session, slot }`. `publish_work` writes **unconditionally**
  to `work[slot]`; `take_solution` reads `solutions[slot]`. No
  gating, no work-id stamping.
- ASIC side: `work_receiver(slot)` hands out one `DynReceiver` per
  slot; `submit_solution(slot, solution)` routes into
  `solutions[slot]`.

Each board picks its own session-slot count as a compile-time const
in its `main.rs`, carried upstream by the generics:

```
pub(crate) const POOL_SLOTS: usize = ubos_firmware::MAX_POOLS;  // f407: = 1
static ASIC_SESSION: AsicSession<POOL_SLOTS> = AsicSession::new();
```

The config schema stays at `MAX_POOLS` capacity fleet-wide (one
schema hash, one REST API), so a board with
`POOL_SLOTS < MAX_POOLS` — stm32f407, whose 128 KiB main SRAM cannot
afford the extra per-slot session-task futures — persists pool
entries it never runs: they validate, store, and render as Dead on
the dashboard. Accepted limitation; runtime capacity reporting in
the API is a follow-up. `host-runner` and unit tests instantiate
small `N`s directly.

### The switch lives in `dispatch_work`

`ubos-mining`'s serial worker holds a receiver for **every** slot. On
each work tick, `dispatch_work` asks its scheduler which slot to
serve and takes the latest assignment from that slot's watch via
`try_get()` (the exact loop is in the work-scheduler section):

- A pool switch becomes effective at the next work tick, bounded by
  the serial work-ticker period, regardless of anything on the pool
  side. No republish machinery, no activation signals.
- When nothing is dispatchable (no slot has a job, or every populated
  slot is excluded), the tick is skipped and the loop keeps servicing
  UART reads and commands instead of stalling; the chip simply idles
  until some slot produces work.
- The work registry entry (`WorkSlot`) records the session slot at
  dispatch time. When a chip response maps back to a registry entry,
  `process_solution` submits via `submit_solution(slot, solution)` — the
  solution returns to the session that created the work, even if the
  scheduler moved on while the chip was hashing. In-flight shares of
  a currently unselected slot are therefore still submitted over its
  (still open) connection; nothing is dropped at a switch boundary.

### `work_id` stays a plain `u32`

No slot bits are packed into `work_id`. Routing information lives
where it is known: the dispatch registry records the slot, and the
submit path passes it as an explicit parameter. Each session keeps
its own independent counter; ids from different sessions never meet
in the same ring, so cross-session collisions cannot occur.

For the record, counter wrap is harmless even for small counters: a
solution lookup only probes the 8-deep `WorkRing` of outstanding
work, so a wrapped id would have to collide with one of the 8 live
entries — publishes are ~1/s, 2^32 apart. No `WorkId` struct is
needed; the id stays opaque.

### Stale-work protection

When a session tears down (connection lost, config change), it
`clear()`s its slot's work watch. `dispatch_work`'s `try_get()` then
skips the slot instead of re-dispatching a dead pool's last job. The
chip keeps sweeping its last dispatched work and goes idle after; the
scheduler treats an empty work cell as "slot unavailable" and falls
back to the best available slot (see the work-scheduler section).

### `run_stratum_client` / `run_stratum_session` (stratum-client)

Both take a `PoolHandle` instead of `&'static AsicSession`, plus the
slot index. The session loop shape (select3) is unchanged —
switching added nothing to it:

- `set_pool_alive(slot, ...)` replaces the hardcoded
  `PRIMARY_POOL_INDEX`.
- `record_reconnect_attempt(slot)`: the function-local
  `FIRST_ATTEMPT` static becomes a per-slot `[AtomicBool; 8]` sized
  to the alive-bitmap index space, resolving the documented
  single-caller invariant. Shares, rejects, and reconnects are
  counted per slot and exposed as `pool`-labelled prometheus
  series; see the per-pool observability section below.

### `ubos-config`: config-generation broadcast

`STRATUM_RESTART: Signal<CSMutex, ()>` is replaced by a generation
counter `Watch<CSMutex, u32, N>` bumped after each successful config
update. Each pool task holds a receiver and treats a generation
change as "tear down the session and re-snapshot". Capacity covers
`MAX_POOLS` receivers plus one spare. The public API is
`Handle::subscribe_generation() -> GenerationSubscription`.

### `ubos-firmware`: per-slot session loop

`run_stratum_session(stack, slot, session, shared)` stays a plain
`async fn` (the esp32s3 executor-version constraint is unchanged);
`shared: StratumShared` is the `Copy` fan-out of the boot singletons
returned by `init_stratum(seed)`. Per-slot loop:

1. Snapshot config; `snapshot.pool.get(slot)`. `None` → await the
   next config generation, retry. Permanent for
   `slot >= MAX_POOLS`, so boards keep `POOL_SLOTS <= MAX_POOLS`.
2. Build the `TcpConnector` from task-local buffers (`let mut
   rx_buffer = [0u8; STRATUM_TCP_BUFFER_SIZE]`): locals held across
   `.await` live in the statically allocated task future, so each
   session instance owns its pair with no shared-static bookkeeping.
3. Protocol dispatch as today. The secp context is initialised once
   and shared as `&'static CryptoCtx` (the SV2 client takes it by
   `&`). The SV2 PRNG becomes per-slot: one boot seed stored by
   `init_stratum(seed)`, each task derives its own `ChaCha20Rng` with
   `set_stream(slot)` — independent streams, still continuous across
   reconnects within a task.
4. `select(one_session, generation_change)` as today; teardown clears
   the slot's work watch.

The `pub static ASIC_SESSION` export is removed. Each board's
`main.rs` owns the static (as `host-runner` already does) and passes
`&'static BoardAsicSession` explicitly to both `serial_task` and the
stratum task shells. The storage itself must remain `static` — no
alloc — but no library crate exports a global.

### Boards and host-runner

Each board's one-line stratum task shell gains
`#[embassy_executor::task(pool_size = MAX_POOLS)]` and `slot` +
`session` parameters; `main` spawns it once per slot in a loop.
`serial_task` gains the session parameter. `host-runner` keeps its
local static (now `AsicSession<1>` or the board alias) and uses
`pool_handle(0)`; its CPU-mock ASIC consumes `work_receiver(0)` and
submits with slot 0.

## Work scheduler (UBOS-72)

The multipool mechanism above first shipped policy-free: the
dispatcher read a manually set active slot that nothing ever set.
The scheduler replaces that with a policy splitting delivered work
between the configured slots at a fixed ratio — the dev-fee use case
— while routing around unavailable pools.

### Accounting unit: difficulty-1 shares

The scheduler does not count dispatch ticks; it counts what the chip
actually produced. Every chip response that passes the difficulty-1
check in `process_solution` (the same point that feeds the hashrate
EWMA via `record_share`) accounts one share to the slot recorded in
the work registry — `WorkSlot::session_slot`, the slot whose
dispatch produced the work. One diff-1 share stands for an expected
2^32 hashes, so the accumulated counts measure real work delivered
per pool, independent of tick period, chip pipeline lag, and job
changes.

### The `Scheduler` trait

Defined in `ubos-mining` (dispatch is its only caller):

```rust
pub trait Scheduler<const N: usize> {
    /// Account one difficulty-1 share found on work dispatched
    /// for `slot`.
    fn accumulate_share(&mut self, slot: usize);
    /// Pick the slot to dispatch next, restricted to `available`
    /// slots. `None` when nothing is dispatchable.
    fn evaluate(&mut self, available: &[bool; N]) -> Option<usize>;
}
```

Both call sites live inside the single `Serial::run` task —
`evaluate` at the top of `dispatch_work` on each work tick,
`accumulate_share` in `process_solution` right after the diff-1
`record_share`, before the pool-target check, so all real chip work
counts. The scheduler is therefore a plain `&mut` value owned by the
serial worker: no atomics, no locking, no shared accumulator type.
`Serial::run` gains a `Sch: Scheduler<N>` generic and takes the
scheduler by value.

### `RatioScheduler<N>`: clamped deficit counters

State: `weights: [u32; N]`, `credit: [i32; N]`, and the precomputed
`weight_sum`. On `accumulate_share(s)`:

```
credit[i] += weights[i]   for every i
credit[s] -= weight_sum
clamp every credit to ±CLAMP
```

`evaluate` returns the available slot with the highest credit among
those with `weight > 0`; ties break to the lowest index.

Why this shape, and not gross lifetime counters or EWMA rates:

- **Exact convergence.** The update is zero-sum, so while no credit
  touches the clamp the delivered share split converges exactly to
  `weights`: the deviation stays bounded by a constant, so the ratio
  error after `n` shares decays as `1/n`.
- **Time-translation symmetry.** Credits hover within one
  `weight_sum` of zero in steady state, so a 1:1 ratio alternates
  slots on every accounted share indefinitely — the switching
  cadence depends only on the bounded credit state, never on how
  long the miner has been running.
- **Bounded outage repayment.** Gross lifetime counters (argmin of
  `count/weight`) build unbounded debt while a pool is down: after a
  day-long outage the returning pool would be mined exclusively for
  a day. The clamp caps repayment at `CLAMP` worth of shares (sized
  to roughly a thousand shares, minutes of catch-up), so post-outage
  behavior is indistinguishable from post-boot behavior. This is the
  one deliberate deviation from exact lifetime ratio.
- **EWMA share rates** would also be time-symmetric but converge
  only asymptotically and need a decay constant plus fixed-point
  bookkeeping; the deficit counter needs integer adds and compares.

### Weight 0 = backup-only

A slot with weight 0 is never picked while any weighted slot has
work, but is picked (lowest index first) when no weighted slot is
available. Weights come from the pool config (see the config-driven
weights section): an unweighted pool list posts every weight as 0,
which the all-zero fallback resolves to "mine slot 0, the rest are
failover backups" — the previous "mine pool 0" behavior. A default
of equal weights would instead silently split hashrate 33:33:33
between pools the operator configured as backups.

### Dispatch integration

`dispatch_work` builds an availability mask and lets the scheduler
choose:

```rust
let mut available: [bool; N] =
    core::array::from_fn(|i| work_receivers[i].contains_value());
loop {
    let Some(slot) = scheduler.evaluate(&available) else {
        return; // nothing dispatchable, skip the tick
    };
    if let Some(assignment) = work_receivers[slot].try_get() {
        // dispatch `assignment`, record `slot` in the registry
        break;
    }
    // The session, which may run on another core, cleared its
    // cell between the mask build and the read. Retry without
    // this slot.
    available[slot] = false;
}
```

`contains_value()` peeks without cloning the `WorkAssignment`; only
the chosen slot pays for the clone. Availability comes from the work
cells themselves — a torn-down session has `clear()`ed its cell —
not from `pool_alive_bitmap()`, which is API-facing state.

### `AsicSession` sheds the `active` atomic

`active: AtomicU8`, `set_active`, and `active_slot` are removed:
their only callers were `dispatch_work` and tests, and the scheduler
owned by the serial worker is now the single decision authority.
`AsicSession` becomes pure per-slot channels. Surfacing "currently
mined pool" to the API/webui is a metrics-gauge follow-up, not
session state.

### Wiring

Each board's `mining.rs` builds `RatioScheduler::new()` and hands it
to `SERIAL.run`; the scheduler starts from the fallback default and
the config-weights watcher pushes the configured weights at boot
(see the config-driven weights section). `host-runner`'s CPU mock
reads `work_receiver(0)` directly, but its miner-serial path also
drives `SERIAL.run` with a `RatioScheduler::new()` for its
single-slot session. The `POOL_WEIGHTS` board consts that first
carried the weights are retired.

### Per-pool observability

Shares and stats are attributed to their pool slot in both the
prometheus exposition and the logs:

- `stratum-client` counts accepted shares, rejected shares, and
  reconnect attempts in per-slot arrays sized by
  `POOL_INDEX_SPACE = 8` (the alive-bitmap index space). The
  existing metric names carry a `pool` label — one series per board
  slot: `board_shares_accepted_total{pool="0"}`,
  `board_shares_rejected_total{pool="0"}`,
  `board_stratum_reconnects_total{pool="0"}`. No unlabelled
  duplicates are emitted; the REST `system_info` totals sum the
  arrays in code, so its schema is unchanged.
- `ubos-mining` counts delivered shares, in units of 2^32 expected
  hashes, per chain and per originating slot (the `pool_shares`
  array in `ChainStats`, bumped next to the scheduler's
  `accumulate_share`) and publishes
  `board_pool_work_shares_total{chain="0",pool="0"}` — the ratio
  between these series is the split the scheduler actually
  delivered. The hashrate EWMA and the share count are per chain as
  well. Each `ChainStats` owns a `HashrateEstimator`, and the two
  series are `board_hashrate_hps{chain="0"}` and
  `board_shares_total{chain="0"}`. They describe chip output, not
  pool credit, so a machine total is the sum over the `chain` label.
- Logs carry `pool {slot}` at the session and state layers: the
  `StratumState` visitor lines (new job, accepted, rejected,
  session updates), the submit and solution-drop lines in `run.rs`,
  and the serial worker's `Submitting share` line. The stratum-v1
  and stratum-v2 protocol clients stay untagged; their lines are
  bracketed by the tagged `pool {slot}` lifecycle logs of the
  owning session.

### Scheduler testing

Scheduler unit tests (host-buildable like the rest of
`ubos-mining`'s): 1:1 alternation on every share, 2:1 convergence
over a long run, bounded catch-up after a simulated outage, weight-0
backup behavior, all-unavailable returns `None`, all-zero weights
rejected. Serial tests drive selection through a fixed-slot stub
scheduler; one test feeds a diff-1 chip response and asserts
`accumulate_share` fires for the originating slot, another proves
dispatch falls back to an available slot when the preferred cell is
empty.

## Config-driven weights (UBOS-72)

The scheduler first shipped with compile-time `POOL_WEIGHTS` consts
in the board crates. This change moved the weight source into the same
`POST /api/config` call that configures the pools, so the split is
operator-tunable at runtime. Weights travel to the serial actor the
same way every other instruction does — through its command
channel — via a small config-watcher task. Design constraint
(deliberate): no existing function gains an argument; the new
message is intercepted in `run`'s command arm so even
`handle_command` keeps its parameter list.

### Schema: a `weight` field on `PoolEntry`

```json
{"pool":[
  {"host":"...","port":3333,"user":"w1","weight":9,"protocol":{...}},
  {"host":"...","port":3333,"user":"w2","weight":1,"protocol":{...}}
]}
```

- `#[serde(default)] pub weight: u32` — an absent key deserializes
  as 0, so existing callers (the webui, curl scripts) keep their
  exact current behavior: a weightless pool list mines slot 0 with
  the rest as failover backups.
- `PoolEntry::validate` rejects `weight > MAX_POOL_WEIGHT`
  (100 000). The bound exists so the scheduler arithmetic
  (`weight_sum * (CLAMP_SHARES + 1)`) can never overflow `i32`
  even with every slot at the maximum — a bad POST earns a 4xx,
  never a firmware panic.
- Adding the field shifts `Config::SCHEMA_HASH`, so the first boot
  of the new firmware discards the stored config and starts from
  defaults (the crate's designed fallback; there is no migration
  machinery). Accepted: one-time re-POST per board, early-stage
  project.

### Weight semantics

Unchanged from the scheduler design, restated against the config
source: weight 0 means backup-only; the all-zero fallback (slot 0
treated as weight 1) is applied **after truncating the config's
`MAX_POOLS` weights to the board's `POOL_SLOTS`**, inside
`set_weights`, so a reduced-slot board whose only weighted entry
lies beyond its slot count still mines slot 0.

### Propagation: a config watcher pushes over the command channel

A dedicated task in `ubos-mining` (next to the scheduler; the crate
already depends on `ubos-config`) subscribes to the config
generation and forwards the mapped weights to the actor whenever
they actually change:

```rust
pub async fn run_pool_weights_watcher() -> ! {
    let handle = ubos_config::handle();
    let mut generation = handle.subscribe_generation();
    let mut last_sent: Option<[u32; 8]> = None;
    loop {
        let weights = pool_weights(&handle.current().pool);
        if last_sent != Some(weights) {
            SERIAL.set_pool_weights(weights).await;
            last_sent = Some(weights);
        }
        generation.changed().await;
    }
}
```

- `pool_weights` maps the pool list to `[u32; 8]` (entry weight per
  slot, absent entries 0) — a pure helper, unit-testable.
- The `last_sent` guard means unrelated config updates (thermal,
  fan) never disturb the scheduler; only an actual pool or weight
  edit reaches it.
- The first iteration runs at boot and delivers the initial weights
  before awaiting the first generation change.
- `Serial::set_pool_weights` is fire-and-forget,
  wrapping a new `SerialMessage::SetPoolWeights([u32; 8])`. This
  keeps the serial actor config-agnostic: weights arrive as a
  command like `Reset` or `StartSendingWork`, and `run`'s command
  arm applies the variant to the scheduler directly before
  delegating everything else to `handle_command`, whose parameter
  list is unchanged.
- Each board (and host-runner) spawns the watcher through the same
  one-line task-shell pattern as the metrics publishers.
- The watcher consumes the `GENERATION_WATCH` spare receiver, so
  the capacity bumps from `MAX_POOLS + 1` to `MAX_POOLS + 2` to
  keep one spare.

### `set_weights`: clean slate on reconfiguration

The `Scheduler` trait gains one method with a default no-op body,
so existing impls and test stubs compile untouched:

```rust
fn set_weights(&mut self, _weights: &[u32]) {}
```

`RatioScheduler`'s impl truncates to its `N`, applies the all-zero
fallback, recomputes `weight_sum` and the clamp, and zeroes every
credit. Resetting the accumulated shares is deliberate: a pool
reconfiguration coincides with the sessions tearing down on the
same generation bump, so the whole pool machinery restarts
coherently and the ratio converges from scratch. The method is
total — inputs are bounded by config validation, and a defensive
saturation keeps direct misuse from overflowing.

`RatioScheduler::new(weights)` and the per-board `POOL_WEIGHTS`
consts are retired: the constructor takes no arguments and starts
from the fallback default (slot 0 weighted), which the watcher's
boot-time push immediately replaces with the configured values. In
the boards, the consts and constructor arguments disappear and one
watcher spawn line appears.

### Webui

The config form has no weight control. Its row order is the
priority order: the first pool is the primary and the others are
backups, tried in order. On Save the form sends each entry as
`{host, port, user, protocol}` with no `weight`, so every weight is
0 and the all-zero fallback mines the first slot. A Save therefore
replaces any ratio configured over the API; a ratio split is set
through `POST /api/config` only.

### Rejected alternatives

- **Pull-based polling inside `Serial::run`** (the actor holds its
  own generation subscription and re-reads the config at the top of
  its loop): smallest diff — no new task, message, or board spawn
  lines — but the chip-driving actor gains a hidden reach into the
  global config handle instead of receiving policy through its
  command API like everything else. Rejected for actor hygiene;
  behavior, cost, and latency are equivalent.
- **Shared weight atomics polled by dispatch**: splits the
  scheduler's source of truth and puts shared mutable state into
  the dispatch hot path.
- **Schema migration**: versioned blobs or dual-decode to carry old
  configs across the hash change; far larger than the feature, and
  the one-time reset is acceptable at this stage.

### Testing

- `ubos-config`: `weight` default on a weightless JSON entry;
  `MAX_POOL_WEIGHT` bound rejected with the per-slot message;
  round-trip through `ConfigUpdate`.
- `ubos-mining` scheduler tests: `set_weights` truncation and
  post-truncation fallback; credit reset (accumulate, reweight,
  assert clean convergence); clamp recomputation; default no-op on
  a stub.
- The `pool_weights` mapping helper is unit-tested directly
  (weights per slot, absent entries, weightless list). The watcher
  loop and command delivery are a few lines over already-tested
  primitives; the end-to-end path is exercised on hardware
  (reweight via `pool_conf.sh`, watch the split move in the Pool
  work split panel).

## SV2 pubkey encoding (base58check in the pool URL, UBOS-78)

The SV2 authority public key is presented as its SV2-spec base58check
string everywhere an operator touches it, rather than as raw bytes.

### Wire model

The config carries a single field, `protocol.Sv2.pool_pubkey`, an
`Option`. Its presence *is* the trust state: `Some` →
`ValidateSignature` against the key, `None` → `ValidateNothing`. There
is no separate "trust" flag on the wire.

### base58check format

`base58( prefix ‖ key ‖ checksum )`, where the prefix is
`SV2_PUBKEY_PREFIX = [1, 0]`, `key` is the raw 32-byte xonly key, and
the checksum is the first four bytes of the double SHA-256 of
`prefix ‖ key` (Bitcoin-style Base58Check). The factory key decodes to
the existing `DEFAULT_POOL_SV2_PUBKEY`. The codec lives in
`ubos-config/src/pubkey.rs` and is alloc-free: base58 goes through
`bs58::{encode,decode}(..).onto(&mut [u8])` into fixed stack buffers,
with the checksum computed directly via `sha2`. `bs58` carries no
`check`/`alloc` feature, so the firmware needs no allocator.

### Storage stays bytes; only JSON speaks base58

`sv2_pubkey_serde` on `pool_pubkey` branches on
`is_human_readable()`: JSON (REST, `serde-json-core`, `true`) carries
the base58check string, while postcard (flash, `false`) carries the
raw `Option<[u8; 32]>` byte-identically. The stored Rust type is
unchanged, so the postcard schema hash does not move and a stored
config survives the change without a reset — the `EXPECTED_BLOB`
golden test guards this. A malformed key over REST fails deserialize
and returns HTTP 400.

### Web UI: key in the URL, front-end-only trust checkbox

Per the SV2 URL form, the key is the URL path segment —
`stratum2+tcp://host:port/<base58>` — not a separate field. The "Trust
pool" checkbox is front-end only (never transmitted): it is an
explicit confirmation gate so an operator cannot end up with no
signature validation by simply forgetting to paste a key. It only
relaxes the *requirement* for a key; a key that is present is always
honored:

| Trust | Key in URL | Sent | Result |
|-------|-----------|------|--------|
| off | present | `"9awt…"` | validate |
| off | absent | reject on save | must add key or tick Trust |
| on | present | `"9awt…"` | validate (key wins) |
| on | absent | `null` | ValidateNothing |

### Clean break

REST no longer accepts the legacy JSON byte-array form for
`pool_pubkey` — the base58check string is the only accepted shape.
`pool_conf.sh` sends the string. Acceptable at this project stage; no
dual-accept transitional code.

## RAM cost

The dominant per-slot cost is the statically allocated session-task
future (`pool_size = POOL_SLOTS` copies): each holds the 2 × 2 KiB
TCP buffers, the protocol-client state, a config snapshot, and the
`StratumState` — roughly 10–13 KiB per slot, measured on stm32f407
where the 3-slot build overflowed 128 KiB RAM by ~25.7 KiB and the
1-slot build links with margin. Each slot also adds a
`WorkAssignment`-sized watch cell and a solution channel (~150 B).
The cost scales with the board's `POOL_SLOTS`, not with how many
pools are configured.

## Rejected alternatives

- **Publish-side gating with slot-stamped `work_id`** (earlier draft:
  handles publish into one shared watch only while active, slot
  packed into the id's top bits, activation signal + republish select
  arm so a switch takes effect under `ntime_roll_period = None`):
  consumer-side selection makes all of that machinery unnecessary —
  sessions stay policy-free, the session loop keeps its select3, and
  no bits are packed into ids. Costs one watch per slot and a
  slot-aware dispatch path in `ubos-mining`, which is where the
  decision belongs anyway.
- **Active-takes-all solution draining** (single shared solution
  channel, the active session drains everything): loses in-flight
  shares on every switch and risks cross-ring `work_id` collisions
  producing garbage submits.
- **Router task** between sessions and the serial worker: an extra
  task and hop with no benefit over per-slot channels.
- **mpsc queue for work delivery**: the ASIC wants the latest job,
  not a backlog; `Watch` is the correct primitive.
- **Supervisor/semaphore respawn loop**: embassy tasks are static;
  one long-lived task per slot with an internal reconnect loop
  maintains exactly one connection per endpoint by construction.

## Testing

Host tests:

- `stratum-client` (`src/tests/asic_session.rs`): per-slot
  publish/take round-trip through `PoolHandle`; solutions submitted
  for slot k reach only slot k's channel; watch `clear()` on
  teardown.
- `ubos-mining` (host-buildable test targets): `dispatch_work` serves
  the scheduler-chosen slot, skips the tick when nothing is
  dispatchable, records the slot in the registry, and routes the
  resulting solution back to the originating slot after the
  scheduler's pick changes mid-flight.

Firmware verification: per-crate `cargo check` + `clippy` through the
`nix develop` shells for every board crate and `host-runner`.
