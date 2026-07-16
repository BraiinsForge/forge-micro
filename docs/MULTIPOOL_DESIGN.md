# Multipool: parallel stratum sessions with scheduler-driven dispatch

> **Status.** Implemented: the multipool mechanism under UBOS-49
> ("Add second stratum channel"), the work scheduler under UBOS-72.
> Step tracking in `/IMPLEMENTATION_PLAN.md`. Kept aligned with the
> code in `ubos/stratum-client`, `ubos/ubos-mining`, and
> `ubos/ubos-firmware` — when the two disagree, the code wins and
> this file needs a patch.

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
  `handle_read` submits via `submit_solution(slot, solution)` — the
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
check in `dispatch_response` (the same point that feeds the hashrate
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
`accumulate_share` in `dispatch_response` right after the diff-1
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
available. The default board weights `[1, 0, 0]` therefore keep the
previous "mine pool 0" behavior and turn the remaining configured
entries into classic failover backups. A default of equal weights
would silently split hashrate 33:33:33 between pools the user
configured as backups. Constructing a `RatioScheduler` with all
weights zero is rejected.

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

Each board's `main.rs` declares `POOL_WEIGHTS: [u32; POOL_SLOTS]`
next to `POOL_SLOTS` (default `[1, 0, 0]`; stm32f407 `[1]`); its
`mining.rs` builds `RatioScheduler::new(POOL_WEIGHTS)` and hands it
to `SERIAL.run`. `host-runner`'s CPU mock reads `work_receiver(0)`
directly, but its miner-serial path also drives `SERIAL.run` and
passes `RatioScheduler::new([1])` for its single-slot session.
Runtime-configurable weights are a follow-up: only the constructor
call changes.

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
- `ubos-mining` counts delivered diff-1 shares per originating slot
  (`POOL_WORK_SHARES`, bumped next to the scheduler's
  `accumulate_share`) and publishes
  `board_pool_work_shares_total{pool="0"}` — the ratio between
  these series is the split the scheduler actually delivered.
  `board_shares_total` and the hashrate EWMA stay global: they
  describe chip output, not pool credit.
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
`nix develop` shells for all seven boards and `host-runner`.
