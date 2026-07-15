# Multipool: parallel stratum sessions with a runtime-switchable active slot

> **Status.** Implemented (UBOS-49, "Add second stratum channel");
> step tracking in `/IMPLEMENTATION_PLAN.md`. Kept aligned with the
> code in `ubos/stratum-client`, `ubos/ubos-mining`, and
> `ubos/ubos-firmware` — when the two disagree, the code wins and
> this file needs a patch.

## Goal

Run one stratum session per configured pool entry (up to
`MAX_POOLS = 3`) in parallel, all connected at the same time, with
exactly one *active* slot feeding work to the ASIC. The active slot is
switchable at runtime from any task via an async-safe API. Selection
happens on the consumer side: sessions always publish their latest
job; the serial worker decides which slot's job to dispatch. This is
the mechanism the dev-fee scheduler will later build on.

## Non-goals

- The dev-fee *scheduler policy* (a task time-slicing between slots at
  a duty cycle). Follow-up branch; this design only provides
  `set_active`.
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
    active: AtomicU8,
}
```

- `set_active(slot)` / `active_slot()`: a plain atomic store/load —
  the runtime switch, callable from any task. No signalling toward
  the sessions; they are policy-free and never know whether they are
  active.
- Host side, per slot: `pool_handle(slot) -> PoolHandle` — a `Copy`
  value `{ session, slot }`. `publish_work` writes **unconditionally**
  to `work[slot]`; `take_solution` reads `solutions[slot]`. No
  gating, no work-id stamping.
- ASIC side: `work_receiver(slot)` hands out one `DynReceiver` per
  slot; `submit_solution(slot, solution)` routes into
  `solutions[slot]`.

`ubos-firmware` re-exports a board-facing alias pinned to the config
capacity, with a static assertion tying the two together:

```
pub type BoardAsicSession = AsicSession<{ ubos_config::pool::MAX_POOLS }>;
```

`host-runner` and unit tests instantiate small `N`s directly.

### The switch lives in `dispatch_work`

`ubos-mining`'s serial worker holds a receiver for **every** slot. On
each work tick, `dispatch_work` reads `active_slot()` and takes the
latest assignment from that slot's watch via `try_get()`:

- A switch becomes effective at the next work tick, bounded by the
  serial work-ticker period, regardless of anything on the pool side.
  No republish machinery, no activation signals.
- `try_get()` replaces today's blocking `get().await`: if the active
  slot has no job yet (pool still connecting, or cleared — see
  below), the tick is skipped and the loop keeps servicing UART reads
  and commands instead of stalling. Today's code blocks on the first
  assignment; after this change the chip simply idles until the
  active slot produces one.
- The work registry entry (`WorkSlot`) gains the session slot index,
  recorded at dispatch time. When a chip response maps back to a
  registry entry, `handle_read` submits via
  `submit_solution(slot, solution)` — the solution returns to the
  session that created the work, even if the active slot changed
  while the chip was hashing. In-flight shares of a deactivated slot
  are therefore still submitted over its (still open) connection;
  nothing is dropped at a switch boundary.

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
future scheduler should consult `pool_alive_bitmap()` before
activating a slot.

### `run_stratum_client` / `run_stratum_session` (stratum-client)

Both take a `PoolHandle` instead of `&'static AsicSession`, plus the
slot index. The session loop shape (select3) is unchanged —
switching added nothing to it:

- `set_pool_alive(slot, ...)` replaces the hardcoded
  `PRIMARY_POOL_INDEX`.
- `record_reconnect_attempt(slot)`: the function-local
  `FIRST_ATTEMPT` static becomes a per-slot `[AtomicBool; 8]` sized
  to the alive-bitmap index space, resolving the documented
  single-caller invariant. The prometheus counter stays a single
  total; per-pool share/reconnect labels are a follow-up.

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
   next config generation, retry.
2. Build the `TcpConnector` from per-slot static TCP buffers
   (`[ConstStaticCell<[u8; STRATUM_TCP_BUFFER_SIZE]>; ...]`, indexed
   by slot).
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

## RAM cost

Per-slot TCP buffers are static: every board pays
2 × 2 KiB × 2 extra slots = 8 KiB RAM even with one pool configured,
plus ~1–2 KiB per additional *running* session (`StratumState`, task
arena) and one extra `WorkAssignment`-sized watch cell plus solution
channel per slot (~150 B each). Accepted; lazy buffers are not
possible without alloc.

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

- `stratum-client` (`src/tests/run.rs`): per-slot publish/take
  round-trip through `PoolHandle`; solutions submitted for slot k
  reach only slot k's channel; watch `clear()` on teardown.
- `ubos-mining` (host-buildable test targets): `dispatch_work` reads
  the active slot's watch, skips an empty active slot, records the
  slot in the registry, and routes the resulting solution back to the
  originating slot after an active-slot change mid-flight.

Firmware verification: per-crate `cargo check` + `clippy` through the
`nix develop` shells for all seven boards and `host-runner`.
