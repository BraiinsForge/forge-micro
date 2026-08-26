---
paths:
  - "**/*.rs"
---

# Locks

Five rules, the traps behind them, and the objects that already follow
them. They came out of an audit of every mutex in this tree and in the
internal repository that consumes it; the changes that audit produced have
landed, so what is left here is the doctrine.

## The rules

1. **A mutex is a private field of the object that owns the invariant.**
   The object exposes methods. A caller reaches the guarded data only
   through a typed accessor that also runs the setup the data needs, never
   through the raw guard. A mutex behind a public accessor is a protocol
   every caller must remember; a mutex behind a method is one the compiler
   enforces.
2. **The authoritative check is under the lock.** A check outside the lock
   is a hint, whatever the atomic ordering says. A flag read before the
   lock may reject early, but the answer a caller acts on is the one it
   reads after the lock is held. One storage location stays the truth: the
   fix is a second read, not a second copy.
3. **The primitive names the access pattern.** One writer with many
   readers is an `RwLock`. A slot written once is a `OnceLock`. A
   synchronous operation is a `blocking_mutex`, never an async `Mutex`.
4. **A lock over `()` is a serialiser and needs a recorded reason.** The
   reason names the two callers it separates and the result if they
   interleave. Better still, give it the state the callers hand on to each
   other, and it stops being a lock over `()` at all.
5. **No await under a lock the caller cannot bound.** A retry loop inside
   a locked span multiplies the worst case by its budget. Measure the span
   before holding a lock across one. A supply bring-up that waits for a
   rail to settle and then writes with retries can span seconds, and a
   lock held across it delays every other caller of that lock by the whole
   span, including the one that cuts power. Re-reading a flag after each
   await costs nothing and keeps every span short.

## Primitives

The workspace pins `embassy-sync 0.8`, so `OnceLock`, `LazyLock`,
`Semaphore` and `RwLock` are all available beside `Mutex` and
`blocking_mutex`.

**Read a `OnceLock` with `try_get`, never `get`.** `get` returns a future
that wakes itself and returns `Pending` until the slot is filled, so it
busy-polls the executor. `try_get` is one relaxed load and answers either
way.

CAUTION: `OnceLock::init` stores the value and then marks the slot ready
with a relaxed store, and `try_get` reads that flag relaxed too. A relaxed
store does not publish the value to another core, so a second core can in
principle see the flag set before it sees the value. Every use here is safe
only because the board starts its second core after the install, and a core
start orders memory between the two. A new `OnceLock` read from more than
one core must record that reason at the site, as `CATALOGS` in
`ubos-events/src/lib.rs` does.

## Choosing the raw mutex

`NoopRawMutex` for tasks on one executor. `CriticalSectionRawMutex` when a
second core or an interrupt can reach the data. The choice is a claim about
the board and nothing gates it, so document it at the site.

`ubos-psu` exports one fixed `RawMutex`, so a board spells what the driver
demands rather than choosing. That is deliberate: a critical section costs
tens of cycles and the I2C transaction under it costs hundreds of
microseconds, so a per-board choice saves under a tenth of a percent and
buys a per-board proof that no gate holds.

Where the fixed choice hides a contradiction rather than costing cycles,
settle it by naming the executor of every consumer. A
`CriticalSectionRawMutex` nested over a `NoopRawMutex` bus is two claims
about how many cores reach the same peripheral, and both cannot be true. If
the outer claim is the real one, the inner lock excludes nothing and two
cores can drive the peripheral at once.

## Holding a guard across an await

No lint catches this today, so review is the gate.
`clippy::await_holding_lock` names the `std` and `parking_lot` types only.
`clippy::significant_drop_tightening` is not the substitute it looks like:
every `embassy-sync` guard does carry `#[clippy::has_significant_drop]`,
but that lint finds a guard whose scope can be narrowed and cannot see one
held across an await, because the guard is in use at that point.

The lint that could is `clippy::await_holding_invalid_type`, which is
stable and takes a type list:

```toml
await-holding-invalid-types = ["embassy_sync::mutex::MutexGuard"]
```

It is not enabled. It fires on every PSU driver method, where the design
requires the guard, so turning it on means an allow at each of those sites.
Taking that trade, or recording why the allows cost more than the lint
catches, is an open decision.

## The objects to copy

- **`ubos_psu::state::Shared<D>`** — the lock, the termination flag and the
  init gate behind one `acquire`, which no caller can spell partly. Rules 1
  and 2 in one object.
- **`ubos_mining::serial::Serial`** — one task owns the resource and the
  others send it tagged messages. Prefer this shape where the trait allows
  it. The `Psu` trait takes `&self`, so the PSU drivers cannot be it and a
  lock is the right answer there. The tag is the part to copy for any
  request-and-acknowledgement pair: without it, a caller dropped mid-wait
  hands its acknowledgement to the next one, which is the interleaving the
  serialising lock was there to prevent.
- **`ubos-ota`'s `shared_flash.rs`** — `blocking_mutex` plus `RefCell` for
  synchronous hardware, which is rule 3's answer for a GPIO or a flash
  peripheral.
