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
4. **Handles, not statics.** Tasks must not talk to each other through
   ad hoc global statics. A handle is the one way that a task talks to a
   task of another crate, and it isolates the API of that crate.
   - **Scope.** A handle is for the communication between tasks and for
     nothing else. If another way is simpler, use that way. State that
     several tasks share and that has no task of its own, such as the
     core-1 park, is a plain type with private fields. The firmware owns
     it as a `static` in `main.rs` and passes a `&'static` reference at
     spawn time. `ubos-events` and `ubos-metrics` are logging, as `defmt`
     is, and helper libraries such as `ubos-auth` and `ubos-core` have no
     task, so these crates keep their global access.
   - **Shape.** Each subsystem that runs a task exposes one `…Handle`.
     The module owns its state in private statics. The handle holds no
     data, so it is a `Copy` token, and its methods are the API of the
     module. Consumers take the concrete handle, not a trait. Nothing
     outside the module reads or writes a static.
   - **Runner.** `Handle::take` returns the handle and a `Runner`. The
     runner is the right to run the task of the subsystem. It is not
     `Copy`, and the task function borrows it or takes it by value, so a
     firmware that gives it to one task cannot give it to a second. What
     only the task does is a method of the runner. The net state has
     several writers, so it returns a `Copy` `Publisher` for the writes,
     and the readers keep the `Handle`. The pool counters have no
     runner: the session tasks of the crate count in them directly, and
     the handle serves the readers.
   - **Where a handle is made.** A firmware takes each handle once at
     the start of `main`, through `Handles::take` of `ubos-firmware`. It
     passes copies of the handles to the tasks that use them and gives
     each runner to its task. `take` is the only constructor, and a
     second `take` panics where it runs. A take on a path that the boot
     does not reach panics when that path first runs. A `main` reads each
     handle as a field of the set, so a handle has one name from the take
     to the task.
   - **The PSU.** Its task is generic over the board's driver, which
     the handle is not, so the handle and the runner follow the same
     rule as the others and the `PowerSupply` holds only the driver. A
     board with no supply takes the handle and spawns no task, so a read
     answers `None` and a command waits.
   - **Tests.** A test takes the handles as a firmware does. A process
     can take a handle once, so each test that takes one runs in a forked
     process through `rusty-fork`. A test that only reads a handle may
     share the one copy that it took through a `OnceLock`. The cost of
     the fork is accepted, because a constructor that exists for tests
     would be a second way to make a handle.
5. **Pure core, thin shell.** Module decision logic lives in pure
   functions. The embassy task is an I/O wrapper.
6. **Glue lives in one place.** Subsystems do not know each other
   exist. They receive handles for whatever they need.
7. **Snapshots at boundaries, mutation inside.** Pass `Config`
   snapshots. Do not share a `Mutex<Config>`.
8. **Safety is separate from control.** The thermal safety trip is
   not the same module as the fan PID loop.
9. **Typed units at module APIs.** `Frequency`, `Voltage`, `Power`,
   `Current`, `Efficiency`, `FanPwm`, `FanRpm`, and `Temperature` are
   newtypes in `ubos-core`. `Ratio` is the one exception. It lives in
   `ubos-metrics`, because only a metric states a ratio.
10. **Uniform module template.** Every subsystem that runs a task
    follows the same skeleton: private `STATE_WATCH` and
    `COMMAND_CHANNEL` statics, a public `Command` enum with domain
    variants, a public `State` snapshot, a `Copy` handle, a runner, and
    one `run` task. The handle is the API. The statics are not. The
    commands of the PSU are the methods of its handle over a private
    `Command` enum. Thermal takes readings where another module takes
    commands: its handle submits each reading to a private input
    channel, and its state is one private watch for each filtered
    temperature that it publishes.
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
    tuning a `ubos-mining` concern over the tuner trait in `ubos-tuner`.
    Control flows one way: config does not import the manager, and the
    manager does not import config.
14. **Ack is opt-in.** A fire-and-forget handle method queues a
    command and returns `()`. A handle method that must return a
    `Result` carries an internal `Signal<Result>` and a serialising
    `Mutex<()>`, with one call in flight per handle. Where a caller can
    be dropped while it waits, as the miner's PSU commands can, the
    command and its reply carry a sequence number, so the next caller
    discards a reply that answers the dropped one.

## Communication rules

All traffic between the tasks of different modules flows through
`…Handle` methods. The set of edges is the sum of the handle surfaces,
so the crate docs are the contract: a module that a handle does not
connect does not talk. A shared type that the firmware owns under
principle 4 is the one other edge, and its crate docs name every
task that holds it.

- **One writer per `STATE_WATCH`** — the module's own `run` task.
- **Only the manager writes `halt`/`resume`.** Every other producer
  influences mining by sending a `Message` into the manager. The
  message vocabulary is in `ubos-manager/src/message.rs`.
- **The API talks only to handles.** No handler reads another crate's
  atomics or statics.
- **The metrics push channel is the one sanctioned shared static.**
  Any layer can push into it through `Metric::push`, and there is
  exactly one consumer. The static is private to the store of
  `ubos-metrics`.
- **`PauseReasons` records intent; `MiningState` records lifecycle status.**
  The manager publishes the pause mask before the miner completes a halt.
  OTA sends `OtaBegin` to the manager and then waits for `Stopped`
  through `ubos_mining::Handle::wait_stopped`.
  The OTA engine separately parks all serial workers before flash writes.
- **Hardware settings stay with their owners.** Chain statistics expose
  each serial actor's confirmed frequency. The PSU exposes the shared
  rail setpoint. The miner does not copy these into lifecycle status.

## Reload and restart

Tasks observe configuration changes through `ConfigHandle` subscriptions.
Most tasks apply changes without a teardown.
The miner applies target edits within `miner.tuning` at the next policy step.
A change between `Fixed` and `Power` during initialization takes effect when work starts.
The same change during mining restarts mining after the current plan completes.

Stratum owns a TCP connection to a specific pool.
Its outer loop selects on a restart signal from `ubos-config`, drops the inner future,
and restarts with the new configuration snapshot.
