# Architecture

Status: implemented — the code wins. When this file and the code
disagree, the code is right and this file needs a patch.

This file holds the cross-crate law: the design principles and the
rules for how modules talk. It names no module API and no wire table.
Each crate's `//!` docs carry its own surface, `ubos-manager`'s docs
carry the pause contract, and `docs/STRUCTURE.md` carries the crate
map and the dependency direction.

## Design principles

1. **Tree-shaped ownership.** All long-lived module state lives under
   a single root constructed in `main.rs`. Sub-tasks borrow `'static`
   handles. No `Arc<Mutex<…>>` cycles between siblings.
2. **One writer per piece of state.** Multiple readers are fine. When
   two modules want to write the same thing, one owns it and the other
   sends commands.
3. **Three communication primitives.** `Watch<T>` for latest-wins
   broadcast, `Channel<T>` for queued events with one consumer, and
   `Signal<T>` for edge-triggered single-shot. Atomics are an
   implementation detail of `Watch` for `Copy` scalars. Do not expose
   an atomic across a crate boundary.
4. **Handles, not statics.** Each subsystem exposes one `…Handle`
   struct: a small `Copy` value over `&'static` references to the
   module's private primitives. Consumers take the concrete handle,
   not a trait. The firmware passes handles at spawn time.
5. **Pure core, thin shell.** Module decision logic lives in pure
   functions. The embassy task is an I/O wrapper.
6. **Glue lives in one place.** Subsystems do not know each other
   exist. They receive handles for whatever they need.
7. **Snapshots at boundaries, mutation inside.** Pass `Config`
   snapshots. Do not share a `Mutex<Config>`.
8. **Safety is separate from control.** The thermal safety trip is
   not the same module as the fan PID loop.
9. **Typed units at module APIs.** `Frequency`, `Voltage`, `Power`,
   `FanPwm`, `FanRpm`, and `Temperature` are newtypes in `ubos-core`.
10. **Uniform module template.** Every subsystem follows the same
    skeleton: private `STATE_WATCH` and `COMMAND_CHANNEL` statics, a
    public `Command` enum with domain variants, a public `State`
    snapshot, a `Copy` handle, and one `run` task. The handle is the
    API. The statics are not.
11. **One consumer per channel.** Exactly one task, `run`, drains each
    module's `COMMAND_CHANNEL` and owns that module's policy. Slow
    work delegates inside the module via a sibling `Signal` or worker
    future, never via a second public channel.
12. **Domain messages, not bit-manipulation.** Senders construct facts
    or intent, for example `DangerousTemperature` or `Stop`. The
    receiving module interprets. No public `Assert(Reason)` or
    `Clear(Reason)` scaffolding crosses a crate boundary.
13. **Concern separation is by module.** Configuration is a
    `ubos-config` concern, pause aggregation a `ubos-manager` concern,
    tuning a `ubos-mining` concern. Control flows one way: config does
    not import the manager, and the manager does not import config.
14. **Ack is opt-in.** A fire-and-forget handle method queues a
    command and returns `()`. A handle method that must return a
    `Result` carries an internal `Signal<Result>` and a serialising
    `Mutex<()>`, with one call in flight per handle.

## Communication rules

All cross-module traffic flows through `…Handle` methods. The set of
edges is the sum of the handle surfaces, so the crate docs are the
contract: a module that a handle does not connect does not talk.

- **One writer per `STATE_WATCH`** — the module's own `run` task.
- **Only the manager writes `halt`/`resume`.** Every other producer
  influences mining by sending a `Message` into the manager. The
  message vocabulary is in `ubos-manager/src/message.rs`.
- **The API talks only to handles.** No handler reads another crate's
  atomics or statics.
- **The metrics push channel is the one sanctioned `pub static`.**
  Any layer can push and there is exactly one consumer.
- **`PauseReasons` is intent; `MiningState.running` is ground truth.**
  They can disagree transiently, because the manager publishes the new
  mask the moment it issues `halt` and does not wait. The asymmetry is
  load-bearing: thermal cannot afford to wait.

## Reload and restart

Every task except stratum reloads in-band: it holds a `ConfigHandle`
and re-reads on `Watch::changed()`. There is no teardown. Stratum is
the exception because its outer state is a TCP connection pinned to a
pool. Its outer loop selects on a restart signal from `ubos-config`,
drops the inner future, and re-enters with the fresh snapshot.
