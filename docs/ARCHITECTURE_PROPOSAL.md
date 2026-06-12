# Rusty Boards — Modularization Proposal

Status: high-level design. Scope: `boards/ubos-main` workspace.

The brief: name the modules, define their public surface, and pin down
how they talk. This document is intentionally high-level — internal
loops, error variants, and test plans are deferred to implementation.
The migration phasing lives in **Appendix A**; the crate inventory in
**Appendix B**.

---

## 1. Where we are today

### What is good

- **Metrics bus.** `static METRICS: Channel<_, Metric, 32>` with a single
  consumer task and many producers calling `push(metric).await`. This is
  the model we want everywhere control-plane state crosses a module
  boundary.
- **AsicSession.** `Watch<WorkAssignment>` (latest-wins, stratum → chip)
  + `Channel<Solution>` (chip → stratum) — the right split between
  "broadcast the freshest job" and "queue every nonce."
- **Crate-per-concern split for drivers.** `ubos-psu`, `ubos-temperature`,
  `ubos-net`, `stratum-client`, `stratum-v[12]`, `mining-protocol` are
  each a coherent unit.
- **Layered driver pattern.** Hardware-specific code lives in the
  firmware crates; `ubos/*` is portable.

### What is broken

| Issue | Where | Effect |
|---|---|---|
| `ubos-monitor` does temperature sensing **and** PID fan control **and** thermal safety signaling in one task | resolved by the split into `ubos-thermal` (`run_thermal`) and `ubos-cooling` (`run_cooling`) | Three independent concerns share one loop |
| `TEMPERATURE_C: AtomicU32` declared, read by API, **never written** | resolved by routing the API read through `ThermalHandle::current()` and removing the atomic | Dashboard always shows 0 °C |
| No manager/supervisor | resolved by introducing `ubos-manager` (`run_manager` aggregates `PauseReasons` and drives `MiningHandle::halt`/`resume` on the empty/non-empty edge) | No central pause/resume, no way to act on config changes |
| `STOP_MINING`, `MINER_IS_RUNNING` signals have no producer/consumer | resolved by deleting `MINER_IS_RUNNING` in favor of `MiningHandle::state()` and wiring `STOP_MINING` to host-runner's SIGINT handler and the work-loop selects | Pause API is a logging stub |
| API reaches into atomics of 5+ crates directly | `ubos/ubos-api/src/system.rs:25-36` | HTTP layer pinned to every subsystem's internals |
| No tuner — `INIT: &[Step]` is a captured trace replayed verbatim, voltage hardcoded `1400 mV` | `ubos/ubos-mining/src/replay.rs` | Cannot set frequency or voltage at runtime |
| No config persistence — `POST /api/config` returns 200 and does nothing | `ubos/ubos-api/src/rest.rs:379` | Web UI changes are placebo |
| `ubos-net` exposes `pub static`s for `LOCAL_IPV4`, `BTC_PRICE_CENTS`, `POOL_ALIVE_BITMAP`, … | `BTC_PRICE_CENTS` resolved by removing the BTC ticker and its static; the rest remain (`ubos/ubos-net/src/net.rs:131,139`) | Shared mutable state crosses every boundary |
| Multi-pool wiring planned but bit 0 hardcoded | `ubos/stratum-client/src/run.rs:66` | Single point of failure |
| Watchdog feeds unconditionally | `ubos/ubos-core/src/watchdog.rs` | A hung task never reboots the system |

The pattern: **shared mutable state via `pub static` atomics is the
default**, used for both "publish for observers" and "tell another
module what to do." We push these toward explicit channels or watch
endpoints owned by *one* writer, then make every consumer go through
that endpoint.

---

## 2. Design principles

1. **Tree-shaped ownership.** All long-lived module state lives under a
   single root constructed in `main.rs`. Sub-tasks borrow `'static`
   handles. No `Arc<Mutex<…>>` cycles between siblings.
2. **One writer per piece of state.** Multiple readers fine. If two
   modules want to write the same thing, they don't — one owns it and
   the other sends commands.
3. **Three communication primitives, used consistently:**
   - `Watch<T>` — latest-wins broadcast.
   - `Channel<T>` — queued events with one consumer.
   - `Signal<T>` — edge-triggered single-shot.
   - Atomics are an implementation detail of `Watch` for `T: Copy`
     scalars; do not expose them across crate boundaries.
4. **Handles, not statics.** Each subsystem exposes a `…Handle` struct
   — a small `Copy` value carrying `&'static` references to the
   module's private primitives. Other modules take the concrete
   `…Handle` (no trait); the firmware passes them at spawn time.
5. **Pure core, thin shell.** Module decision logic lives in pure
   functions. The embassy task is an I/O wrapper.
6. **Glue lives in one place.** The manager is the only module allowed
   to import the others. Subsystems do not know each other exist; they
   receive handles for whatever they need.
7. **Snapshots at boundaries, mutation inside.** Pass `Config`
   snapshots; don't share `Mutex<Config>`.
8. **Safety is a separate concern from control.** The thermal safety
   trip is not the same module as the fan PID loop.
9. **Typed units at module APIs.** `Frequency`, `Voltage`, `Power`,
   `FanPwm`, `FanRpm`, `Temperature` are newtypes in `ubos-core`.
10. **Incremental migration.** New paths run alongside old until
    `nix flake check` is green; only then delete.
11. **Uniform module template.** Every subsystem follows the same
    skeleton:

    ```rust
    // module-internal — these names are not part of the public surface
    static STATE_WATCH:     Watch<CSMutex, State, N>     = Watch::new();
    static COMMAND_CHANNEL: Channel<CSMutex, Command, M> = Channel::new();

    // public surface
    pub enum Command { /* domain messages, not Assert/Clear scaffolding */ }
    pub struct State  { /* observable snapshot */ }

    #[derive(Clone, Copy)]
    pub struct Handle { commands: &'static Channel<…>, state: &'static Watch<…> }
    pub fn handle() -> Handle;

    pub async fn run(deps: …) -> !;
    ```

    `STATE_WATCH` and `COMMAND_CHANNEL` are internal storage names;
    every cross-module producer/consumer reaches them via the public
    `…Handle` (`send`, `current`, `changed`, `subscribe`, plus any
    module-specific convenience methods). The handle is the API; the
    statics are not. Modules with no inbound commands today still ship
    a `COMMAND_CHANNEL` carrying a domain enum with one or more real
    variants — never empty scaffolding, but the channel is present so
    the shape is uniform across crates. Modules with no outbound
    observable state may skip `STATE_WATCH`. No `…Trait`/`…Handle`
    pair: consumers depend on the concrete handle.
12. **One consumer per channel.** Each module's `COMMAND_CHANNEL` is
    drained by exactly one task, `run`, which owns the policy for that
    module. Slow work delegates inside the module via a sibling
    `Signal` or worker future — never by exposing a second public
    channel. Embassy's MPSC `Channel` already enforces the one-consumer
    rule mechanically.
13. **Domain messages, not bit-manipulation.** Senders construct
    facts or intent (`DangerousTemperature`, `BrokenFan`, `Start`,
    `Stop`); the receiving module interprets. No public `Assert(Reason)`
    /`Clear(Reason)` scaffolding leaking implementation into the wire
    contract.
14. **Concern separation is by module.** Configuration is a
    `ubos-config` concern; pause aggregation is a `ubos-manager`
    concern; tuning is a `ubos-mining` concern. Control flows in one
    direction: `ubos-config` does not depend on `ubos-manager` or
    `ubos-mining`; mining subscribes directly to `ubos_config::handle()`
    and reads the live snapshot, so the manager never imports
    `ubos-config` and config never imports the manager.
15. **Ack default is Signal + serialising Mutex.** Where a handle
    method must return a `Result` (e.g. a config update that can fail),
    the handle carries an internal `Signal<Result>` and `Mutex<()>`
    alongside its public `Channel<Cmd>`. The method takes the mutex,
    sends the command, awaits the signal, releases. Single in-flight
    per handle. Embassy ships no oneshot and the no_std crate
    ecosystem has none without `alloc`; the borrowed-`&Signal` (or
    vendored Tweede-Golf-style) pattern is reserved for the day a use
    case demands concurrent callers. Fire-and-forget commands skip
    this entirely and return `()`.

---

## 3. Module map

Five layers, strictly downward dependencies.

```
Layer 4  firmware/{rp2040, rp2350, bmm-pico, stm32f407, stm32f439, esp32s3}
            │ wires handles, spawns tasks, board-specific HAL
            ▼
Layer 3  ubos-manager  ──►  ubos-api  ◄── ubos-webui, ubos-http, ubos-mdns
            │     ▲               ▲
            │     │ commands       │ commands via ManagerHandle
            ▼     │               │
Layer 2  ubos-mining (+ tuner)  ubos-thermal   ubos-cooling   ubos-config
         │                       │              │               │
         │ stratum-*              ubos-temperature              (uses ekv)
         │ mining-protocol
         ▼
Layer 1  ubos-psu  ubos-temperature  ubos-net  stratum-client  stratum-v1/v2
Layer 0  ubos-core (+ typed units)  ubos-logging  mining-protocol
         ubos-metrics
```

`ubos-metrics` sits at layer 0 so any layer can `push`; its single
consumer task is spawned by the firmware.

Crate inventory (new/renamed/deleted/folded) is in **Appendix B**.

---

## 4. Module APIs

Each subsystem follows the uniform template from §2 principle 11: a
private `STATE_WATCH` (when observable state exists), a private
`COMMAND_CHANNEL` (when inbound commands exist), a public `Command`
enum, a public `State` struct, a `Copy` `…Handle`, and a single `run`
task that drains `COMMAND_CHANNEL`. Consumers depend on the concrete
handle; no public trait. Subsystems do not import each other except
through the handles passed in at spawn time.

### `ubos-thermal`

Sensor poller; publishes the single source of truth for board
temperature, and owns the edge logic between `safe` and `dangerous`
thresholds. Multi-reader by design — cooling, the tuner inside mining,
and the API subscribe to the temperature `STATE_WATCH`. The manager
does not subscribe; thermal pushes events into the manager's
`COMMAND_CHANNEL` when edges cross.

```rust
static STATE_WATCH:     Watch<CSMutex, ThermalReading, N> = Watch::new();
static COMMAND_CHANNEL: Channel<CSMutex, ThermalCmd, M>   = Channel::new();

pub enum ThermalCmd {
    // populated when a real inbound command exists (e.g. SetThresholds
    // for runtime config). Until then ships with a single noop variant
    // to keep the shape uniform — empty enums break Channel.
    Noop,
}

pub struct ThermalReading {
    pub chip_max:     Temperature,
    pub board:        Temperature,
    pub timestamp_ms: u64,
    pub sensor_fault: bool,
}

#[derive(Clone, Copy)]
pub struct ThermalHandle {
    state:    &'static Watch<CSMutex, ThermalReading, N>,
    commands: &'static Channel<CSMutex, ThermalCmd, M>,
}

pub async fn run<S: Sensor>(sensor: S, manager: ManagerHandle) -> !;
```

`run` polls `sensor`, publishes each reading on `STATE_WATCH`, and
emits `ManagerMessage::DangerousTemperature` / `TemperatureNormal` on
the manager's `COMMAND_CHANNEL` whenever the chip-max crosses the
configured threshold. Edge detection lives here, not in the manager —
the manager doesn't know about thresholds.

### `ubos-cooling`

Fan PID task + fan HAL traits. Subscribes via `ThermalHandle` for PID
input. Sends `ManagerMessage::BrokenFan` / `FanRestored` via
`ManagerHandle` on tach failure / recovery. No safety-trip logic here.

```rust
pub trait FanOutput  { fn set_pwm(&mut self, pwm: FanPwm); }
pub trait TachReader { fn read_rpm(&mut self) -> FanRpm; }

pub async fn run<F: FanOutput, T: TachReader>(
    fan:     F,
    tach:    T,
    thermal: ThermalHandle,
    manager: ManagerHandle,
) -> !;
```

### `ubos-mining` (rename of `ubos-miner`)

Chip driver, serial actor, work-dispatch loop, and tuner. The work loop
is a thin executor of tuner commands; chip bringup lives in the tuner
module beside the work loop.

```rust
static STATE_WATCH:     Watch<CSMutex, MiningState, N> = Watch::new();
static COMMAND_CHANNEL: Channel<CSMutex, MiningCmd, M> = Channel::new();

pub enum MiningCmd {
    Halt,
    Resume,
}

pub struct MiningState {
    pub running:    bool,
    pub chip_count: u8,
}

#[derive(Clone, Copy)]
pub struct MiningHandle {
    state:    &'static Watch<CSMutex, MiningState, N>,
    commands: &'static Channel<CSMutex, MiningCmd, M>,
}

// Miner::new(psu, reset_pin).run().await  — entry point
pub fn new<P: Psu, R: ResetPin>(psu: P, reset_pin: R) -> Miner<P, R>;
pub async fn run(self) -> !;
```

`MiningHandle::halt` and `resume` are fire-and-forget: they queue a
`MiningCmd` variant and return as soon as the channel slot is taken.
Callers that need to observe completion subscribe to `STATE_WATCH`
and wait for the `running` field to change — the "command ack is
opt-in" rule classifies them as fire-and-forget, so no Signal+Mutex
ceremony is needed here.

`MiningState` is slow-changing operational state only. Share counters
and hashrate live in `ubos-metrics`; putting them here would wake every
`STATE_WATCH` subscriber on every share.

Frequency, voltage, and algorithm are not carried over the mining
command channel. The mining loop subscribes directly to
`ubos_config::handle()` and reads the live config snapshot at the top
of each bring-up. An algorithm change tears down the active tuner
(`tuner.deinit` + rail cut) and rebuilds it; other tuning edits apply
on the next bring-up.

Tuner lives at `ubos_mining::tuner` — `Tuner` trait with `init` /
`deinit` phases, plus `FixedTuner` (the current default, walks a
`V^2 * f` curve to the target frequency and voltage) and `ReplayTuner`
(walks the captured BMM100 trace through `MinerControl`'s byte-level
escape hatches). Constructors take
`&'static ChipDescriptor` and `&'static BoardDescriptor`. The trait
returns `Result<(), TunerError>`. Surfacing tuner faults to the
manager as `PauseReason::TunerFault` is a planned follow-up; the
landed `Miner` loop cuts the rail and retries on an `init` error.

PLL targets are computed by a calculator (integer-N PLL math), not a
baked table. Validation: assert equality against factory dividers in a
host test.

### `ubos-config`

Config schema, persistence, and broadcast. Owns the `Watch<Config>`
writer and the inbound command channel. `Storage` trait with
`sequential-storage` flash impl and file-backed host impl. `postcard`
on flash; `serde_json_core` stays on the HTTP boundary only. Schema is
versioned with an explicit migration chain.

```rust
static STATE_WATCH:     Watch<CSMutex, Config, N>     = Watch::new();
static COMMAND_CHANNEL: Channel<CSMutex, ConfigCmd, M> = Channel::new();

pub struct Config {
    pub pool:    heapless::Vec<PoolEntry, 3>,
    pub thermal: ThermalConfig,
    pub cooling: CoolingConfig,
    pub tuning:  TuningConfig,
    pub api:     ApiConfig,
}

pub enum ConfigCmd {
    Update(Config),
    SetNetwork(NetworkConfig),
    FactoryReset,
}

#[derive(Clone, Copy)]
pub struct ConfigHandle {
    state:    &'static Watch<CSMutex, Config, N>,
    commands: &'static Channel<CSMutex, ConfigCmd, M>,
}

pub trait Storage {
    type Error;
    async fn load<'a>(&mut self, key: &str, buffer: &'a mut [u8])
        -> Result<Option<&'a [u8]>, Self::Error>;
    async fn store(&mut self, key: &str, value: &[u8])
        -> Result<(), Self::Error>;
}

pub async fn run<S: Storage>(storage: S) -> !;
```

`run` is decoupled — it takes no `ManagerHandle` or `MiningHandle`.
It loads from `storage` on boot (or publishes `Config::default()` on
absent/mismatched version), publishes on `STATE_WATCH`, then drains
`COMMAND_CHANNEL`. For each `Update(new)`:

1. `storage.store("config", &postcard::to_bytes(&new))`.
2. Publish `new` on `STATE_WATCH`.
3. Signal stratum restart (sibling `Signal<()>` exposed via the handle).

Mining subscribes directly to `ubos_config::handle()` and reads the
live snapshot at the top of each bring-up; `ubos-config` does not
reach into the manager or mining at all. The slow flash work runs
inside `run` — there is exactly one consumer of `COMMAND_CHANNEL`.

### `ubos-manager`

Aggregates pause reasons and is the sole writer of
`mining.halt`/`mining.resume`. Manager does not own config persistence
and does not know the thresholds that produce events — both are other
modules' concerns. Manager's job is policy: turn incoming events and
operator intent into a `PauseReasons` mask and an edge-triggered call
to mining.

```rust
static STATE_WATCH:     Watch<CSMutex, PauseReasons, N>   = Watch::new();
static COMMAND_CHANNEL: Channel<CSMutex, ManagerMessage, M> = Channel::new();

pub enum ManagerMessage {
    // Condition events from other modules. Each carries explicit
    // edge direction so the manager doesn't have to track who-asserted-what.
    DangerousTemperature,
    TemperatureNormal,
    BrokenFan,
    FanRestored,
    PoolUnreachable,
    PoolReachable,
    TunerFault,
    TunerRecovered,

    // Operator intent from the API.
    Start,
    Stop,
}

#[repr(u8)]
pub enum PauseReason {
    DangerousTemperature = 0,
    BrokenFan            = 1,
    UserRequest          = 2,
    PoolUnreachable      = 3,
    // 4 is deliberately vacant (formerly Reconfiguring; wire positions preserved)
    TunerFault           = 5,
}

pub struct PauseReasons(u16);   // bitmask storage; positions hidden

#[derive(Clone, Copy)]
pub struct ManagerHandle {
    state:    &'static Watch<CSMutex, PauseReasons, N>,
    commands: &'static Channel<CSMutex, ManagerMessage, M>,
}

pub async fn run(mining: MiningHandle) -> !;
```

`run` selects on `COMMAND_CHANNEL` only — no thermal subscription, no
config channel, no second task. Each message maps to a single bit
transition in the internal `PauseReasons`; on the empty↔non-empty
edge `run` calls `mining.halt()` / `mining.resume()` and publishes the
new mask on `STATE_WATCH`. `Start`/`Stop` map onto
`PauseReason::UserRequest`.

`PauseReason` is the manager's internal vocabulary for the mask; the
wire contract is `ManagerMessage`. Senders never spell `PauseReason`.

Single consumer, single channel, single task — slow work (flash) is
not on this path because config owns its own task. The thermal trip
path is one wake on `COMMAND_CHANNEL` and one call to mining.

Every variant of `PauseReason` is halt-worthy: any non-empty
`PauseReasons` mask means mining is halted. `PoolUnreachable` is no
exception — when all pool entries are dead it asserts, mining halts,
and the edge transition back to empty (whichever entry reconnects
first) resumes. There is no observation-only pause reason; pool-side
status that does not warrant a halt belongs in `MiningState` or a
future stratum handle, not in this enum.

Config validation happens at the HTTP boundary before the
`ConfigCmd::Update(new)` send: out-of-range fields and schema
mismatches surface as a 4xx response. Flash failures reported
post-send are visible via the next `STATE_WATCH` value (configured vs
last-known) — a future `ConfigCmd::Save` with the Signal+Mutex ack
shape can return a `Result` if a hard handshake is required.

### `ubos-api` (refactor)

Stops importing other subsystems' internals. Takes the four concrete
handles — no generics, no traits.

```rust
pub async fn run(
    stack:      NetStack,
    manager:    ManagerHandle,
    mining:     MiningHandle,
    thermal:    ThermalHandle,
    config:     ConfigHandle,
    board_name: &'static str,
) -> !;
```

The API is a translator from HTTP to module messages — it routes
each request to the right module's channel:

- `GET /api/system/info` composes from `thermal.current()`,
  `mining.state()`, `manager.pause_reasons()`, `config.current().version`.
- `POST /api/config` → `config.commands.send(ConfigCmd::Update(new))`.
- `POST /api/system/pause` → `manager.commands.send(ManagerMessage::Stop)`.
- `POST /api/system/resume` → `manager.commands.send(ManagerMessage::Start)`.

No direct atomic reads. No `Watch<SystemState>` aggregator. The API
holds three handles and never bypasses them.

### `ubos-net` (slim)

Keep DHCP, NTP, and `connect_stratum`. (TLS and the BTC price
ticker were removed with the `ubos-pool` crate.) Remove the grab-bag
of `pub static` atomics; values move to `ubos-metrics` or a narrow
`NetHandle` if a non-metrics consumer needs them.

---

## 5. Communication contract

A line in this table is a contract. If it's not on the list, the
modules don't talk. Each line names the producer, the consumer
module's channel/watch, and the payload.

All cross-module traffic flows through `…Handle` methods. The
`STATE_WATCH` / `COMMAND_CHANNEL` statics named in §4 are
module-private implementation; consumers never name them.

| From | To | Handle method | Payload |
|---|---|---|---|
| Stratum client | Mining (serial) | `Watch<WorkAssignment>` (AsicSession) | latest job |
| Mining (serial) | Stratum client | `Channel<Solution>` (AsicSession) | nonces |
| Any producer | Metrics task | `Channel<Metric>` | metric events |
| Thermal `run` | Cooling, Mining (tuner), API | `ThermalHandle::current` / `changed` | latest `ThermalReading` |
| Thermal `run` | Manager | `ManagerHandle::send` | `DangerousTemperature` / `TemperatureNormal` |
| Cooling `run` | Manager | `ManagerHandle::send` | `BrokenFan` / `FanRestored` |
| Pool client | Manager | `ManagerHandle::send` | `PoolUnreachable` / `PoolReachable` |
| Tuner (mining module) | Manager | `ManagerHandle::send` | `TunerFault` / `TunerRecovered` |
| API | Manager | `ManagerHandle::send` | `Start` / `Stop` |
| Manager `run` | All readers | `ManagerHandle::pause_reasons` / `changed` | live `PauseReasons` |
| Manager `run` | Mining | `MiningHandle::halt` / `resume` | edge transitions only |
| API | Config | `ConfigHandle::update` | `Config` |
| Config `run` | All readers | `ConfigHandle::current` / `changed` | latest `Config` |
| Config `run` | Stratum | `ConfigHandle::stratum_restart` (`Signal<()>` wrapper) | restart nudge |
| Config `run` | Mining (via handle) | `ubos_config::handle()` subscription | live `Config` snapshot |
| API | Mining | (none today) | future operator-driven tuning |
| Mining `run` | All readers | `MiningHandle::state` / `subscribe` | `MiningState` |
| Mining `run` | PSU | direct method calls (owned by mining) | enable/disable |
| Cooling `run` | Fan HAL | direct method calls (owned by cooling) | PWM, tach |

Rules:

- **No two crates write the same state.** Every `STATE_WATCH` has one
  writer — the module's `run` task. Bootstrap relies on `Watch::new()`
  being `const`; no install-once dance, no `StaticCell`.
- **One consumer per channel.** Each module's `COMMAND_CHANNEL` is
  drained only by its own `run`. Slow work delegates inside the module
  to a worker future signalled from `run`, never to a second public
  channel. Mechanically enforced by `embassy_sync::Channel` being MPSC.
- **Domain messages on the wire.** Senders construct facts
  (`DangerousTemperature`) or operator intent (`Stop`). No public
  `Assert(Reason)`/`Clear(Reason)` — that scaffolding lives inside the
  manager's mask logic and never crosses the crate boundary.
- **API talks only to handles.** Direct atomic imports go away.
- **One writer for halt/resume.** Only manager `run`. Other producers
  influence mining indirectly by sending events into the manager.
- **Thermal is multi-reader for state, single-target for events.**
  Cooling reads via `ThermalHandle` for PID input; manager hears about
  thresholds only as events sent via `ManagerHandle`.
- **`PauseReasons` is intent; `MiningState.running` is ground truth.**
  They can disagree transiently. Manager publishes the new mask the
  moment it issues `mining.halt()` and does not wait for confirmation.
  Readers treat the two values as orthogonal — the asymmetry is
  load-bearing because thermal cannot afford to wait.
- **Command ack is opt-in.** Where an inherent handle method needs a
  `Result` it follows the Signal+Mutex pattern from §2 principle 15.
  Fire-and-forget methods skip the Mutex and return `()`.

### Reload and restart

Every task except stratum reloads in-band: holds a `ConfigHandle` and
re-reads on `Watch::changed()`. No teardown.

Stratum is the exception because its outer state is a TCP connection
pinned to a specific pool. One `select!` at the outer loop:

```rust
loop {
    let pool = config.current().pool.current_entry();
    select! {
        result = run_stratum_client(pool, ...) => handle_result(result),
        _ = restart_signal.wait() => {},
    }
}
```

When the reconfigure task signals `stratum_restart`, the inner future
is dropped, the loop re-iterates, and the new pool config is picked
up. The outer loop re-establishes the invariants on each entry.

### Flow: normal mining (pool → chip → pool)

```
stratum:    receives new job (V1 mining.notify / V2 NewMiningJob)
            decodes; updates JobRing<2>; constructs WorkAssignment
            publish on Watch<WorkAssignment> (AsicSession)

mining:     work loop sees Watch::changed()
            translates to BM1362 frames; writes via serial actor
chip:       (on-board) hashes the work; emits nonce on UART
mining:     serial actor decodes the response; builds Solution
            push on Channel<Solution> (AsicSession)

stratum:    Channel::recv() wakes
            submits to pool (mining.submit / SubmitSharesStandard)
            updates accept/reject counters (metrics + MiningState)
```

This is the steady-state. Watch is latest-wins so an old job in flight
is overwritten the moment a new one arrives; the work loop sees the
fresh `WorkAssignment` on its next `changed()`. Solutions are queued in
the `Channel<Solution>` so no nonce is lost if stratum is briefly
slow.

### Flow: critical temperature

```
thermal run:  read sensor; ThermalReading{chip_max=92°C} crosses threshold
              ThermalHandle: publish ThermalReading
              ManagerHandle::send(DangerousTemperature)
cooling run:  ThermalHandle.changed() → PID drives fan PWM to max
manager run:  receive() wakes → set DangerousTemperature bit
              edge empty → non-empty: MiningHandle::halt()
              ManagerHandle: publish PauseReasons
mining run:   receive Halt → psu.disable() → MiningState.running=false
api:          composes /api/system/info from per-handle reads

(later) thermal: chip_max=44°C crosses threshold downward
              ManagerHandle::send(TemperatureNormal)
manager run:  clear DangerousTemperature bit
              edge non-empty → empty: MiningHandle::resume()
```

The trip path is three task wakes (thermal `run` send → manager `run`
wake → mining `run` wake → PSU disable). Each hop is a single executor
yield plus a channel wake, so the whole path is bounded well under one
second on every board — inside the hashboard's thermal safety budget.
A side channel routing thermal straight to the mining task was
considered and rejected: it would duplicate the trip path without
measurable latency gain and fragment the "only manager writes
halt/resume" rule from §5.

### Flow: user changes pool

```
api:          POST /api/config → ConfigHandle::update(new)   → 200 OK
config run:   receive Update
              storage.store(...)  [flash — slow]
              ConfigHandle: publish new Config on STATE_WATCH
              ConfigHandle: signal stratum restart
mining:       next bring-up reads live config snapshot via
              ubos_config::handle(); applies new freq/voltage/algorithm
stratum:      select! drops inner future; re-iterates with new pool
```

Config `run` does not reach into the manager or mining command channel.
Mining subscribes directly to `ubos_config::handle()` and picks up the
new config on its next bring-up. If a temperature trip fires
concurrently, thermal sends `DangerousTemperature` to the manager
independently — the manager owns the halt/resume edges, and config
does not need to participate in or observe that path.

---

## 6. Bootstrap (sketch)

```rust
// 1. HAL init (board-specific): clocks, I2C, UART, SPI, network MAC.
// 2. Construct drivers (psu, sensor, fan, tach, storage).
// 3. Obtain module handles. Each module owns its primitives as
//    private statics with const constructors, so the firmware does
//    no install dance. Per module:
//      let thermal = ubos_thermal::handle();
//      let mining  = ubos_mining::handle();
//      let manager = ubos_manager::handle();
//      let config  = ubos_config::handle();
//    AsicSession is constructed here too and threaded by &'static into
//    stratum and mining — not a pub static in either.
// 4. Spawn tasks. Each module's `run` owns the producer side of its
//    private statics; the firmware only passes in driver structs and
//    the handles of modules it depends on:
//      spawner.spawn(ubos_thermal::run_task(sensor, manager));
//      spawner.spawn(ubos_cooling::run_task(fan, tach, thermal, manager));
//      spawner.spawn(ubos_mining::run_task(psu, reset));
//      spawner.spawn(ubos_manager::run_task(mining));
//      spawner.spawn(ubos_config::run_task(storage));
//      spawner.spawn(ubos_api::run_task(stack, manager, mining,
//                                       thermal, config, board_name));
//      plus stratum_client, metrics, net, mdns, display, wdt.
```

### Handle availability at spawn time

There is no chicken-and-egg: every `…Handle` is a `Copy` value over
`&'static` references to module-private statics that exist for the
whole program. Handles work whether the producer task has started or
not — a consumer that calls `…Handle::current` before the first
publish gets `None`; a consumer that calls `…Handle::changed` blocks
until the first publish. No task-spawn ordering is required between
modules.

Seeding the watches before spawning is no longer necessary as a
correctness measure (callers handle the pre-publish window via
`Option<State>` from `.current()`), but config publishes its loaded
or default value on the very first thing its `run` does so that
post-spawn consumers see a populated `Config` immediately. Other
modules whose initial value is meaningful (`MiningState { running:
false, .. }`) publish from `run` before entering the main loop.

`AsicSession` (defined in `mining-protocol`) follows the same rule —
constructed in `main.rs` and passed by `&'static` to both stratum and
mining; it is not a `pub static` anywhere. Nothing outside
`ubos-manager` uses its internals; nothing outside `ubos-mining` uses
the tuner module.

---

## 7. Open questions

- **Per-task watchdog.** Deferred to a separate MR after the manager
  stabilises. Sketch: dedicated feeder task that polls per-task
  heartbeat atomics every ~100 ms; per-task grace windows; manager
  task gets the tightest grace.
- **`PoolStatus` exposure.** Once stratum failover lands (Appendix A,
  Step 8): `Watch<PoolStatus>` on `StratumHandle`, or fields of
  `MiningState` if the API is the only consumer.
- **Thermal command channel content.** The uniform template gives
  `ubos-thermal` a `COMMAND_CHANNEL` even though no inbound command
  exists today. Step 5e will fold runtime threshold tuning into the
  `Config` subscription (no command needed), so the channel may stay
  with a single `Noop` variant indefinitely. Revisit if a real inbound
  command appears (manual recalibrate, force-publish-now, etc.).

---

## References

- matklad, *Software Architecture* (2026); *How to Test* (2021);
  *Unit and Integration Tests* (2022).
- scattered-thoughts, *Reflections on a Decade of Coding* — tree-shaped
  state; one source of truth; explicit event loops.
- Braiins OS (`~/projects/bos-main`) — `PauseState` bitmask, two-phase
  halt, config-version chain, three-layer chip/board descriptor pattern
  are worth copying; tarpc process split, file-polling reload, and
  `OnceLock` metrics singleton are not.
- Jack Ganssle, watchdog timer design articles. Elecia White, *Making
  Embedded Systems*, §5 (watchdog patterns).

---

# Appendix A — Migration plan

Nine steps. Each ends with `nix flake check` green. Steps marked `*`
introduce behaviour changes and need sign-off before merging.

> **Note on traits.** Earlier drafts of Steps 2–5 introduced
> `ThermalSource`, `MiningControl`, `Manager`, and `ConfigSource` traits
> as decoupling seams between subsystems. They were dropped after the
> migration landed: the embassy-backed handles are the only impls and
> the trait surface added indirection without buying generic reuse. The
> final code uses inherent methods on the concrete `…Handle` types. The
> step descriptions below preserve the trait-based migration sequence
> as historical record; the diff between the intermediate and final
> state is one cfg-free cleanup commit per affected crate.

### Step 1 — Typed units into `ubos-core`

Add `Frequency`, `Voltage`, `Power`, `Temperature`, `FanPwm`, `FanRpm`.
Migrate call sites. Add to the `checks` matrix. Mechanical.

### Step 2* — Split `ubos-monitor`

Move sensor reads into `ubos-thermal`; expose the `ThermalSource` trait
and the embassy-backed `ThermalHandle` impl. Move PID and fan output
into `ubos-cooling` (fan HAL traits live here); cooling becomes
generic over `Th: ThermalSource`. Fix the dead `TEMPERATURE_C` bug by
routing the API read through `ThermalSource::current()`. Safety logic
stays in place temporarily (moves in Step 4). Delete `ubos-monitor`.

### Step 3* — Rename `ubos-miner` → `ubos-mining`, introduce `MiningControl`

`MiningControl` trait + embassy-backed `MiningHandle` impl; the impl
funnels trait calls through an internal `MiningCmd` channel to the
work loop and exposes a `Watch<MiningState>` reader. `MiningState`
holds only slow-changing operational state (`running`, `chip_count`);
share counters and hashrate remain in `ubos-metrics`. Frequency,
voltage, and algorithm are not carried over the command channel —
mining subscribes directly to `ubos_config::handle()` and reads the
live snapshot at the top of each bring-up. Boot init still inside
mining temporarily — moves to the tuner module in Step 6. Wire
`HALT_MINING` to a trait `halt` call temporarily; behaviour preserved.
`STOP_MINING` (orderly shutdown with DEINIT replay) defers to Step 4:
the trait exposes only `halt`, the orderly-stop semantic has no trait
surface, and the manager will own shutdown once it lands.

Land in five sub-steps to keep the legacy `STOP_MINING`/`HALT_MINING`
signals operational at every commit:

- **3a — Rename.** `ubos-miner` → `ubos-mining`. No behaviour change.
- **3b — Trait + handle scaffolding.** `MiningControl` trait,
  `MiningHandle` impl, `MiningCmd` enum, `MiningStateChannel` /
  `MiningCmdChannel` aliases. No consumers wired.
- **3c — Publisher wiring.** `miner::run` (since restructured into
  `Miner::new(psu, reset).run()`) takes the `MiningPublisher`
  and the `MiningCmdChannel` reference. Every rail transition
  publishes `MiningState`; the work loop selects across `STOP_MINING`,
  `HALT_MINING`, and the cmd channel. Reader half is constructed but
  discarded at every firmware site.
- **3d — Thermal producer migration.** `run_thermal` becomes generic
  over `Mi: MiningControl`. The four `HALT_MINING.signal(value)` calls
  become `mining.halt().await` / `mining.resume().await`. Firmware
  mains claim the previously-discarded reader, build a `MiningHandle`,
  and thread it to thermal. The `ubos-mining → ubos-thermal` dep edge
  (for `HALT_MINING`) and the new `ubos-thermal → ubos-mining` dep
  edge (for `MiningControl`) cannot coexist — Cargo forbids cycles —
  so `miner.rs` drops its `HALT_MINING.wait()` listener and the
  static itself is deleted from `ubos-thermal` in the same commit.
  Behaviour is preserved within that commit because the cmd-channel
  path (`MiningCmd::Halt`/`Resume`) carries the same edges the signal
  used to carry.
- **3e — Legacy atomic deletion.** Once 3d wires the watch-based
  state path, the redundant `MINER_IS_RUNNING: Signal` and
  `MINER_IS_RUNNING_STATE: AtomicBool` come out of
  `ubos-mining/src/miner.rs`. Consumers shift to `MiningState.running`
  via `MiningControl::subscribe`.

`AsicSession`, `WorkAssignment`, and `Solution` stay in
`stratum-client`. An earlier draft proposed moving them to
`mining-protocol`, but the types are tightly coupled to stratum's
share-submission state: `work_ring` indexes by `work_id` from
`Solution`, and `OutstandingWork` (defined alongside in `work.rs`)
maps `work_id` to the context needed by `submit_share`. Splitting
`Solution` away from `OutstandingWork` would either fracture that
state machine or leave a load-bearing back-reference. The fields of
`WorkAssignment` (extranonce_2, ntime, nbits, merkle root) are also
denominated in stratum's terms — a future SV2 or solo client would
need different fields, so the "neutral boundary" framing is a
fiction. `AsicSession` itself wraps embassy-sync `Watch` and
`Channel`, which would drag the executor dependency into the
protocol crate. Extraction becomes warranted only when a second,
genuinely non-stratum producer of work assignments materialises;
until then `ubos-mining` imports the types from `stratum-client` as
`ubos-miner` does today.

### Step 4* — Introduce `ubos-manager`

One task, one `ManagerHandle`. Module-internal `STATE_WATCH` and
`COMMAND_CHANNEL`. The handle is a `Copy` struct over `&'static`
references; no trait, no `assert/clear` scaffolding. `run(mining)`
drains `COMMAND_CHANNEL`, folds each `ManagerMessage` into the
internal `PauseReasons`, edge-triggers `mining.halt`/`resume`, and
publishes the new mask.

Land in two sub-steps so manager decisions can be compared against
existing behaviour before going authoritative.

- **4a — Shadow.** Spawn `ubos_manager::run_task(mining)`
  observe-only: it folds events into `PauseReasons` and publishes
  `STATE_WATCH`, but does not call `mining.halt/resume`. Thermal keeps
  calling `MiningHandle::halt/resume` directly (the existing 3d path).
  Manager decisions are logged; diff them against thermal's edges on a
  real board for a settling period before flipping.
- **4b — Authoritative.** Manager takes over `mining.halt/resume`.
  Thermal stops calling `MiningHandle::halt/resume` directly and sends
  `ManagerMessage::DangerousTemperature` / `TemperatureNormal` into
  the manager channel instead. API `POST /api/system/pause` and
  `/resume` send `ManagerMessage::Stop` and `Start`. Cooling sends
  `BrokenFan`/`FanRestored` when wired (likely deferred until the
  cooling crate gains tach monitoring).

### Step 5* — `ubos-config`

`ubos-config` owns persistence and broadcast on its own. Module-private
`STATE_WATCH<Config>` and `COMMAND_CHANNEL<ConfigCmd>`. `ConfigHandle`
is a `Copy` struct over `&'static` references; no trait. `Storage`
trait, `sequential-storage` flash impl, file-backed host impl.
`postcard` for the on-flash format; `serde_json_core` stays on the
HTTP boundary only. 5c's compile-time schema fingerprint guards the
on-flash layout: a mismatch on load falls back to `Config::default()`
and the migration chain stays unwired until a real transform needs
writing. Pool config moves to runtime (`heapless::Vec<PoolEntry, 3>`).
`run(storage, manager)` is the sole `Watch<Config>` writer. `POST
/api/config` becomes real. Compile-time env credentials removed.

`sequential-storage` over `ekv` because the embassy stack is async
end-to-end and `sequential-storage` exposes `embedded-storage-async`
which every target HAL already implements (`embassy-rp::flash`,
`embassy-stm32::flash`, `esp-storage`); `ekv` is sync with its own
custom flash trait. The config keyspace is <20 keys, so
`sequential-storage`'s linear-scan-within-page is fine — the >1000-key
regime where `ekv`'s LSM-tree wins is not in scope.

Land in five sub-steps. Stand up the surface first, defer authority
until consumers exist.

- **5a — Schema + handle scaffolding.** `ubos-config` crate with the
  `Config` struct, sub-types (`PoolEntry`, `ThermalConfig`,
  `CoolingConfig`, `TuningConfig`, `ApiConfig`), `ConfigCmd` enum
  (start with `Update(Config)` only — no speculative variants),
  module-private `STATE_WATCH` and `COMMAND_CHANNEL`, public
  `ConfigHandle: Copy`, and a shadow `run` that publishes
  `Config::default()` and parks. Spawned on every firmware and
  host-runner. No storage, no command processing yet. Sub-types
  defined locally in `ubos-config`; intentional duplication with
  `ubos-thermal::ThermalConfig` and `ubos-cooling::CoolingConfig`
  resolves in 5b.
- **5b — Hoist runtime config types into `ubos-config`; drop the
  manual `version` field.**
  `ThermalConfig` moves from `ubos-thermal` to `ubos-config`;
  `CoolingConfig` and `PidConfig` move from `ubos-cooling` to
  `ubos-config`. Consumer crates import from `ubos-config` and convert
  at the boundary where the schema shape differs: `ubos-thermal`
  converts `sample_interval_ms: u32` to `Duration` at `run_thermal`'s
  call to `Ticker::every`, because `Duration` is not directly
  serde-friendly and `u32` milliseconds is the persisted shape.
  No behaviour change. Lands before storage (5c) so the on-flash
  layout is locked to the final shape — moving a type after data has
  been written would either require a `Config::VERSION` bump (and a
  migration we never wrote) or risk silently mis-decoding stored
  blobs as the new shape.

  The redundant `version: u32` field is removed from `Config` in the
  same step. 5c's compile-time schema fingerprint detects any
  add/remove/rename/reorder/type change automatically, so a manual
  revision number is dead weight and a second authority that disagrees
  with the fingerprint is worse than either alone.
- **5c — `Storage` trait + flash backend + host file impl with schema
  fingerprint.**
  `Storage` trait (async load/store), `sequential-storage` map-mode
  backend, file-backed host impl using `postcard`. Per-board flash
  range constants live in the firmware crates so `ubos-config` stays
  board-agnostic.

  Stored records carry an 8-byte schema hash prefix computed at
  compile time via
  `postcard_schema::key::hash::fnv1a64::hash_ty_path::<Config>(…)`.
  Load reads the prefix, compares against `Config::SCHEMA_HASH`, and
  returns `LoadError::SchemaMismatch` on disagreement; `run` falls
  back to `Config::default()` and logs. Any field addition, removal,
  rename, reorder, or type change shifts the hash and is detected at
  the next boot.

  Unit tests cover four cases: round-trip, schema-mismatch fallback
  (synthesise a blob with a wrong magic prefix), corrupted-postcard
  fallback, and a golden snapshot of `Config::default()` bytes plus
  the expected schema hash. The snapshot is defence-in-depth: a
  developer who alters `Config` and regenerates the golden without
  noticing must consciously commit both the new bytes and the new
  hash, which keeps the change reviewable rather than silent. No
  firmware wiring yet.
- **5d — `run` becomes authoritative.** Reads storage on boot; on
  load error or version mismatch publishes `Config::default()` and
  logs. `run(storage)` takes no `ManagerHandle` or `MiningHandle`.
  Drains `COMMAND_CHANNEL`: on `Update(new)` writes flash, publishes
  on `STATE_WATCH`, signals stratum restart (sibling `Signal<()>`
  exposed via the handle). On `SetNetwork` persists network config
  only. On `FactoryReset` resets to defaults. Mining picks up the new
  config via its direct `ubos_config::handle()` subscription on the
  next bring-up. Firmware mains wire the per-board storage backend
  through.
- **5e — Consumer migration + API + env removal.** `ubos-thermal`,
  `ubos-cooling`, `ubos-mining` and stratum subscribe to
  `ConfigHandle::state` and recompute on `changed()`. Pool config
  moves off `POOL_*` env vars and onto `Config::pool`. `POST
  /api/config` sends `ConfigCmd::Update(new)` directly to
  `ubos-config` (not through the manager) with JSON validation at the
  HTTP boundary. Compile-time `DEFAULT_POOL_*` and
  `DEFAULT_USERNAME`/`DEFAULT_PASSWORD` constants removed; defaults
  live in `Config::default()`.

### Step 6* — Fold tuner into `ubos-mining`

`Tuner` trait with `init` / `deinit` phases, returning
`Result<(), TunerError>`. Land as `ubos_mining::tuner`. Surfacing an
`Err` to the manager as `PauseReason::TunerFault` is a planned
follow-up; the landed `Miner` loop cuts the rail and retries on an
`init` error. `FixedTuner` (the current
default) walks a `V^2 * f` curve to the target operating point.
`ReplayTuner` walks the captured BMM100 trace through `MinerControl`'s
byte-level escape hatches unconditionally — no feature gate. Typed
boot init lands in `mining-protocol::bm1362`. PLL
calculator replaces lookup table; factory-divider validation test
asserts equality for known entries.

### Step 7 — Slim `ubos-net` and refactor `ubos-api`

Remove `pub static` atomics from `ubos-net`; replace with a
`NetHandle: Copy` following the uniform template. Refactor
`ubos-api::run` to take concrete handles
(`ManagerHandle, MiningHandle, ThermalHandle, ConfigHandle, NetHandle`);
delete imports of subsystem internals. Delete the
`ThermalSource`/`MiningControl`/`ConfigSource`/`Manager` traits
introduced in Steps 2–5 and replace each consumer's `impl Trait`
parameter with the concrete `…Handle`. After this step the wire table
in §5 is the complete inter-module contract, and every cross-boundary
call is a method on a `…Handle`.

### Step 8 — Stratum failover

Stratum loops over the three pool entries on disconnect.
`POOL_ALIVE_BITMAP` is replaced by `Watch<PoolStatus>` on
`StratumHandle` (or rolled into `MiningState`). All entries dead →
stratum sends `ManagerMessage::PoolUnreachable` on the manager
channel. Signal-based restart from Step 4 is the mechanism.

### Step 9 — Per-task watchdog

Separate MR after the manager stabilises. Dedicated feeder task,
per-task grace windows, manager on the tightest grace.

---

# Appendix B — Crate inventory

Each crate listed with its reason to exist, internal modules, the
tasks it spawns, and the handles or traits it publishes.

## Existing (keep, minor refactors only)

### `mining-protocol`
Protocol-agnostic mining client trait + borrowed domain types
(`MiningClient`, `JobId`, `Share`, hashing helpers), and the
stratum↔chip channel contract (`AsicSession`, `WorkAssignment`,
`Solution`). Defines the contract between protocol adapters
(`stratum-v1`, `stratum-v2`) and the integration layer
(`stratum-client`), and the contract between `stratum-client` and
`ubos-mining`. Owning the boundary types here means neither side of
either boundary owns the wire between them. Library only — no tasks,
no handles.

### `stratum-v1`
No-std Stratum V1 wire types, parser, line framing. Implements
`mining-protocol::MiningClient`. Library only.

### `stratum-v2`
Stratum V2 adapter wrapping `ii-sv2`. Implements
`mining-protocol::MiningClient`. Library only.

### `stratum-client`
Pool-side integration: owns the TCP connection, `JobRing<2>`,
`WorkRing<8>`, and the generic visitor that drives V1 and V2 through
the same code path.

- **Module `run`** — Task: `run_task`. Outer `select!` loop over pool
  entries; takes `Signal<()>` for restart (signalled by the manager's
  reconfigure task on config change). Consumes `AsicSession` (from
  `mining-protocol`) by `&'static`; does not define it.

### `ubos-psu`
Async PSU driver. `Psu` trait + miniminer BMM100 I²C impl. Owned by
the mining task as a value; no separate handle. Library only.

### `ubos-temperature`
`TemperatureSensor` trait + sensor drivers (LM75A, board die sensors).
Today runs its own periodic sampler that pushes to metrics; under this
proposal the sampling task moves into `ubos-thermal` and this crate
stays as the pure driver layer.

### `ubos-net`
Network framework — DHCP/smoltcp glue, NTP, and the `connect_stratum`
helper. (The BTC price ticker and its TLS plumbing were removed with
the `ubos-pool` crate.)

- **Module `net`** — Tasks: DHCP loop. Today exposes `pub static`s
  for IP and pool bitmap; Step 7 removes them. (The BTC price static
  was already removed with the ticker.)
- **Module `ntp`** — Task: periodic NTP poll.
- After Step 7: a narrow `NetSource` trait + `NetHandle` impl
  carries the few values a non-metrics consumer needs (dotted-quad
  IP for the dashboard).

### `ubos-metrics`
Prometheus exposition + central metric channel + hashrate EWMA +
per-core CPU-load gauge. Sits at layer 0 so any layer can push.

- **Module `metrics`** — Task: exposition HTTP accept loop on the
  Prometheus port; metric consumer task that drains
  `static METRICS: Channel<Metric, 32>`.
- **Module `hashrate`** — Task: EWMA accumulator (driven by the work
  loop pushing share events).
- **Module `cpu_load`** — Task: idle-tick sampler on each core.
- Push API: producers `await metrics::push(metric)`. The single static
  channel is the one allowed exception to the no-pub-static rule —
  any layer can push and there is exactly one consumer.

### `ubos-api`
HTTP API for the dashboard SPA and external clients.

- **Module `dashboard`** — Task: HTTP accept loop (via
  `ubos-http::serve`) on the API port; handlers for `/`, `/logo.svg`,
  `/login`, `/api/info`, `/api/config`.
- **Module `system`** — Handlers for `/api/system/*` endpoints
  (info / pause / resume / restart / identify).
- Refactor (Step 7): the task signature takes the concrete handles —
  `run(stack, manager, mining, thermal, config, board_name)` with
  `ManagerHandle, MiningHandle, ThermalHandle, ConfigHandle`. Handlers
  compose responses from inherent-method reads on the handles; the
  direct atomic imports go away.

### `ubos-http`
Edge-http accept-loop wrapper, response helpers, `BufferWriter` over
`&mut [u8]`. Library — the accept loop runs inside the caller's task.
No tasks of its own.

### `ubos-webui`
Embedded SPA assets (HTML/JS/CSS) compiled into the firmware binary
and served by `ubos-api`. No tasks, no handles.

### `ubos-mdns`
mDNS responder + `ServiceEntry` declaration macro. Crates that own a
port declare a service at module scope; firmware composes the slice
at the `run_mdns` call site.

- Task: `run_mdns` — responder loop bound to UDP/5353.

### `ubos-display`
OLED render task + SH1106 SPI driver.

- **Module `sh1106`** — driver, no task.
- **Module `display`** — Task: periodic render loop, taking the
  concrete handles of the subsystems it shows (`MiningHandle`,
  `ThermalHandle`, `NetHandle`). Today reads cross-board atomics for
  IP, fan PWM/RPM, hashrate; under the new model those reads go through
  inherent methods on the handles. (The BTC price line was removed with
  the ticker.)

### `ubos-logging`
defmt ↔ log macro facade switched by feature flag. Library only.

### `ubos-core`
Subsystem-scoped constants, hardware watchdog feed loop, status LED,
CCM RAM helpers, and (after Step 1) typed units (`Frequency`,
`Voltage`, `Power`, `Temperature`, `FanPwm`, `FanRpm`).

- **Module `consts`** — per-subsystem constants. No task.
- **Module `watchdog`** — `Watchdog` trait + Task: `run_watchdog`
  feed loop. Step 9 refactors this to a per-task heartbeat poll.
- **Module `led`** — Task: status LED blink loop.
- **Module `fan_state`** — today's `FAN_PWM_PERCENT` / `FAN_RPM`
  atomics; deleted in Step 2 when cooling owns this state.

### `ubos-pool` (removed)
Binance BTC ticker JSON parser. (Step 5 moved pool config into
`ubos-config`'s on-flash record store and the `pool_config` module
was removed.) The crate was later deleted with the BTC ticker;
`ubos-net` no longer runs a BTC fetch task.

- **Module `btc`** — Binance ticker JSON extractor, formerly used by
  `ubos-net`'s BTC fetch task.

### `esp32s3-tsens`
ESP32-S3 internal die-temperature sensor driver. Implements
`ubos-temperature::TemperatureSensor`. Library only.

### `serial-baudrate`
Pure helpers: BM1362 baud register encoding, MCU UART divisor math.
Library only.

### `rp235x-sha256`
RP2350 SHA-256 hardware accelerator wrapper. Used by the mining hash
verification path on bmm-pico. Library only.

## New

### `ubos-thermal`
Single source of truth for board temperature; replaces
`ubos-monitor`'s sensing half.

- **Module `sensor`** — Task: `run<S: Sensor>(sensor, manager)`.
  Polls the sensor, publishes each `ThermalReading` on a private
  `STATE_WATCH`, emits `ManagerMessage::DangerousTemperature` /
  `TemperatureNormal` into the manager's channel on threshold edges.
- Public handle: `ThermalHandle: Copy` carrying `&'static` references
  to the private `STATE_WATCH` and `COMMAND_CHANNEL`. Multi-reader on
  state — cooling, the tuner, and the API subscribe.

### `ubos-cooling`
Fan PID + fan HAL; replaces `ubos-monitor`'s actuation half.

- **Module `pid`** — Task: `run<F: FanOutput, T: TachReader>(fan,
  tach, thermal, manager)`. Reads PID gains + target temp from
  `ConfigHandle::state`, drives `FanOutput`, reads `TachReader`,
  pushes both to metrics. Emits `ManagerMessage::BrokenFan` /
  `FanRestored` on tach failure / recovery.
- Traits: `FanOutput`, `TachReader` (board crates implement).
- Public handle: `CoolingHandle: Copy`. State is published via
  metrics; no `STATE_WATCH` today.

### `ubos-config`
Config schema, persistence, and broadcast.

- **Module `schema`** — `Config` type (versioned), `PoolEntry`,
  `ThermalConfig`, `CoolingConfig`, `TuningConfig`, `ApiConfig`.
- **Module `storage`** — `Storage` trait + `sequential-storage`
  flash impl (firmware) + file-backed host impl (tests). `postcard`
  on flash; `serde_json_core` stays on the HTTP boundary only.
- **Module `run`** — Task: `run<S: Storage>(storage, manager)`.
  Task: `run(storage)` — no `ManagerHandle` or `MiningHandle` params.
  Loads on boot, publishes initial `Config` on the private
  `STATE_WATCH`, then drains the private `COMMAND_CHANNEL<ConfigCmd>`.
  Handles `Update` (persist + publish + stratum restart), `SetNetwork`
  (persist network only), `FactoryReset`. Mining subscribes directly
  to `ubos_config::handle()` and reads the live snapshot.
- Public handle: `ConfigHandle: Copy` (state + commands +
  `stratum_restart` signal).

### `ubos-manager`
One task, one handle. Aggregates pause reasons; sole writer of
`mining.halt/resume`.

- **Module `run`** — Task: `run(mining)`. Drains the private
  `COMMAND_CHANNEL<ManagerMessage>`. Each message maps to one bit in
  the internal `PauseReasons` mask; on the empty↔non-empty edge calls
  `mining.halt()` / `mining.resume()`. Publishes the mask on the
  private `STATE_WATCH<PauseReasons>`.
- **Module `pause`** — `PauseReason` enum, `PauseReasons(u16)`
  bitmask storage, `apply_message` pure function (`ManagerMessage ->
  PauseReasons -> PauseReasons`).
- Public handle: `ManagerHandle: Copy` over `&'static` references to
  the private `STATE_WATCH` and `COMMAND_CHANNEL`. No trait.

## Renamed

### `ubos-miner` → `ubos-mining`
Chip driver, serial actor, work-dispatch loop, and tuner — in one
crate because the boundary between "what the work loop does" and
"what the tuner does" is module-level, not crate-level.

- **Module `work_loop`** — Task: `Miner::new(psu, reset).run()`.
  Subscribes to `ubos_config::handle()` and reads the live config
  snapshot at the top of each bring-up. Drives the chip serial actor,
  dispatches `WorkAssignment` from `AsicSession`, returns nonce
  `Solution`s back through `AsicSession`. Drains the private
  `COMMAND_CHANNEL<MiningCmd>` (`Halt`, `Resume`).
- **Module `tuner`** — Driven by `Miner::run`, which calls
  `tuner.init` / `tuner.deinit` imperatively; an `init` error cuts the
  rail and retries. Surfacing tuner faults to the manager as
  `ManagerMessage::TunerFault` is a planned follow-up.
  - `Tuner` trait with `init` / `deinit` phases.
  - `FixedTuner` impl (current default, walks a `V^2 * f` curve).
    `ReplayTuner` replays the captured BMM100 trace, unconditional — no
    feature gate.
  - PLL calculator (integer-N math, no baked table).
  - Boot-init sequencing.
- **Module `serial`** — Chip-side UART actor, BM1362 frame
  encoding/decoding. No task of its own; runs inside `work_loop`.
- Public handle: `MiningHandle: Copy` over `&'static` references to
  the private `STATE_WATCH<MiningState>` and
  `COMMAND_CHANNEL<MiningCmd>`. Inherent methods (`halt`, `resume`)
  wrap channel sends; no trait.

## Deleted

- `ubos-monitor` — sensor reads → `ubos-thermal`; PID → `ubos-cooling`;
  safety logic → `ubos-manager`.

## Folded

- Typed units (`Frequency`, `Voltage`, `Power`, `Temperature`,
  `FanPwm`, `FanRpm`) → `ubos-core`. A separate `ubos-types` buys
  nothing the module boundary doesn't already provide.
- Fan HAL traits (`FanOutput`, `TachReader`) → `ubos-cooling`. No
  separate `ubos-fan`.
- Storage trait + ekv + file backends → `ubos-config`. No separate
  `ubos-storage`.
- Tuner trait + impls + PLL calculator → `ubos_mining::tuner`. Tuning
  is mining-domain knowledge; folding avoids a crate whose only
  consumer is the mining work loop.
