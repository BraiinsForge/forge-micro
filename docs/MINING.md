# ubos-mining backend (BM1370 hashboard)

Status: implemented — the code wins. When this file and the code
disagree, the code is right and this file needs a patch.

`firmware/hashnode` drives a 4-chip BM1370 hashboard through the
workspace-shared `ubos/ubos-mining/` crate. This page covers how the
board wires the driver up.

## Topology

```
+----------------------+    UART1 (BM1370 serial, GPIO4/5)     +--------------+
| firmware/hashnode    |<------------------------------------->| 4-chip       |
|                      |    I2C1 (SIC45x PSU + sensors)        | BM1370       |
| serial_task  (core1) |<------------------------------------->| hashboard    |
| miner_task   (core0) |    GPIO32 (hashboard reset)           |              |
| psu_task             |<------------------------------------->|              |
+----------------------+    GPIO30 (SIC_EN rail gate)          +--------------+
```

The same backend code (the `ubos-mining` and `ubos-psu` crates) runs on the
`host-runner` against a Linux tty and `/dev/i2c-N`, which is how the
driver is exercised without a hashboard on the bench.

## Long-running tasks

`firmware/hashnode/src/mining.rs` spawns two tasks;
`firmware/hashnode/src/psu.rs` adds one more:

| Task | Owner | Role |
|------|-------|------|
| `serial_task` | `mining.rs` | Drives one entry of the board's static `mining::CHAINS` array through `Serial::run(...)`. One actor per hashboard chain: command dispatch, parser, in-flight work registry, baud-rate switch, and that chain's own `ChainStats`. Pinned to Core 1 so embassy-net on Core 0 stays responsive. |
| `miner_task` | `mining.rs` | Drives `Miner::<_, T, CHAINS>::new(psu_handle, BOARD, chains, platform, mining).run(config)` against the tuner `MinerConfig::tuning` selects from the firmware's tuner set `T`. Bring-up / teardown state machine: reset assert, wait until the scheduler would dispatch work, PSU up, reset release, per-chain enumeration, per-chain safe-boot configuration, start work, apply tuner plans, PSU off. |
| `psu_task` | `psu.rs` | Drives `ubos_psu::PowerSupply::run` with the PSU runner. Carries out the rail commands the other tasks send through the PSU handle, records the voltage each command sets, and polls the SIC45x input/output power every `POWER_STATS_INTERVAL` (two seconds) and publishes it. |

The SIC45x rail is gated by the `SIC_EN` GPIO the `BoardPsu` owns, not by
a PMBus register, so there is no separate PSU-heartbeat task; only
`psu_task` drives `SIC_EN`, on the commands `miner_task` sends through
the PSU handle: high during bring-up, and low when mining halts.

`serial_task` and `miner_task` never return under normal operation.
`Miner::run` responds to `Handle::stop` with a rail cut.
HALT and STOP interrupt the state machine, as the section on it says.
After HALT, the machine waits for `Message::Resume` or `Handle::stop`.
Resume restarts bring-up, while STOP ends the machine. On
bare metal there is no `Handle::stop`, so if either task ever returns the
task idles for 60 s between log lines, keeping the rest of the firmware
(network, metrics, watchdog) alive so logs can be pulled.

## Chain driver and scheduler

The serial actor is generic over the chip driver and the work scheduler,
both injected at spawn:

- `ubos_asic::Bm13xxDriver::new(config::CHIP)`, where `config::CHIP` is
  `Chip::Bm1370`, supplies the BM13xx frame layouts and reset timing for
  the concrete chip.
- `ubos_mining::scheduler::RatioScheduler` distributes generated work
  across the configured pool slots by a clamped deficit counter;
  `run_pool_weights_watcher` keeps its weights in sync with config.
  A board with a fee slot names `FeeScheduler` instead, which wraps
  it and holds the board's last slot at the fee's share
  (`docs/DEVFEE.md`).
- `config::BOARD` (`&HashboardDescriptor`) carries the chip-neutral
  hashboard facts the actor and tuner need.
- The `defaults` field of `config::BOARD` (`HashboardDefaults`) is what
  the config task builds a new config from.

## Halt/resume edges via `ubos-manager`

`ubos-manager::run_manager` is the sole caller of `MiningHandle::halt`
and `MiningHandle::resume`. It owns a `PauseReasons` bitmask published
on `STATE_WATCH` and flips edges on the empty/non-empty boundary. One
resume is sent without an edge: a `Start` that finds the mask empty,
which is how an operator restarts a miner that parked itself after a
bring-up found no chain.

| Producer         | `Message` sent to the manager | `PauseReason` bit asserted |
|------------------|-------------------------------|----------------------------|
| `ubos-thermal`   | `DangerousTemperature`        | `DangerousTemperature`     |
| `ubos-thermal`   | `TemperatureUnreadable`       | `TemperatureUnreadable`    |
| `ubos-cooling`   | `BrokenFan`                   | `BrokenFan`                |
| tuner            | `TunerFault`                  | `TunerFault`               |
| OTA (`ubos-api`) | `OtaBegin`                    | `OtaUpdate`                |
| operator         | `Stop`                        | `UserRequest`              |

### The pool gate

Pool reachability is not a pause reason. The miner gates itself: at
boot and after every rail cut it holds the chains in reset with the
rail off and publishes `MiningState::WaitingForPool` until the
scheduler would dispatch work. Availability comes from the work cells
of the `AsicSession`, as in `MULTIPOOL_DESIGN.md`: a torn-down
session clears its cell. The miner asks the chain 0 serial actor,
which owns the receivers and the scheduler; the question touches no
chip. It asks the scheduler because only the scheduler knows which
slots it serves: a job in the fee slot alone is not dispatchable, so
it neither starts the miner nor keeps it running. While mining, a run
in which the scheduler would dispatch nothing for `POOL_LOSS_GRACE`
cuts the rail and returns to the wait, so a pool outage shorter than
the grace is only a dispatch gap.

### Two ways out of an overheat halt

Where a reading survives the rail cut, the thermal loop sees the
cooldown itself and sends `TemperatureNormal`. Where the chips
themselves are the thermometer, the trip cuts the rail those chips
answer on: the board cools where nothing can see it, no reading can
report the cooldown, and only the operator's acknowledgement
(`POST /api/system/thermal/clear`) clears the halt. The API sends
`TemperatureAcknowledged` and then `Start` to the manager itself, the
same way it sends `Start` and `Stop`. Clearing and starting are both
that route's job, so a pause the operator holds comes off with the
halt; the thermal loop reads the halt back from the manager's mask on
every tick, so the release reaches it the same way its own would. A
bare `POST /api/system/resume` while the halt stands is refused with
409, because `Start` alone would clear only the operator's own pause
and leave mining halted.

The web UI reads only the dominant reason, so its Start button leans on
both ends of the severity order. That order is `PRIORITY` in
`ubos/ubos-manager/src/pause.rs`, which `PauseReasons::dominant` walks
and a const assertion holds in step with `PauseReason::severity`. The
table above lists the producers, not the order. `UserRequest` is
reported only when nothing else holds the mask, and
`DangerousTemperature` is reported in front of anything else. The API
refuses on the mask bit itself. The two agree while those two reasons
keep their places.

The unreadable halt is different from both: the loop retracts it
itself, once a reading returns or the board goes idle, and no operator
control exists for it because there is nothing to acknowledge. A
sensor that never answers again -- a `SensorArray` on a dead I2C bus,
for example -- is a hardware fault. The halt stands until the bus
answers, and a reboot does not cure it: the loop spends its tolerance
budget again and halts again.

Domain producers raise typed messages; they never name a `PauseReason`.
Manager translates and serialises the halt/resume sequence so mining
sees one logical halted-state regardless of how many pause reasons are
active concurrently.

The mining command channel carries `Message::Halt` and `Message::Resume`.
The active miner ignores redundant Resume messages.
After Halt, the miner waits for Resume or `Handle::stop`.
A Resume queued after Halt remains available to that wait.

Frequency, voltage, and algorithm edits do not travel the command channel.
`Miner::run` subscribes to the configuration handle that it receives.
The miner reads the latest frequency and voltage target before each policy step.
These edits do not restart mining or interrupt initialization.
The state machine below sets when an algorithm change takes effect.
Other configuration changes do not restart mining.

## Mining state machine

One `Miner` owns the PSU rail and every chain of the machine.

`Miner::run` first cuts the rail: it puts every chain in reset and disables the PSU.
If the PSU-disable command fails, it logs the error and continues with bring-up.

`Miner::run` is a state machine on the `transitions` crate. Each state is a type.
Its `run` function does the work of the state and returns the next state.
The return type names every state that the state can go to.
A test checks that every state is reachable from `Choose` and has a path back to it.
The states are:

1. `Choose`: check that the firmware's tuner set has a tuner for `miner.tuning`.
   If it has none, go to `NoTuner` and hold until the configuration selects another tuning, then check again.
2. `Reset`, `WaitForPool`: assert reset on every chain and hold it until the scheduler would dispatch work.
3. `Enable`, `ExitReset`: enable the PSU and release reset, with a settle delay before and after each.
4. `BringUp`: reset, enumerate, and configure all chains concurrently.
   Retry failed attempts, then park absent or faulted chains in reset.
   Read chip temperatures before a chain becomes present.
   If no chain comes up, go to `Recover`, or park in `Halted` once `BRING_UP_CYCLES` cycles in a row found none.
5. `StartWork`: build the tuner from the configuration as it is now, set the open-core voltage,
   and start work on every present chain.
6. `Step`: read the latest target, feed readings to the tuner, bound its plan, and apply it.
7. `Pause`: wait for the plan delay, then go to `Step`.
   An algorithm change ends the wait and goes to `Choose`, which starts initialization with the new policy.
   A scheduler with nothing to dispatch for `POOL_LOSS_GRACE` also goes to `Choose`, which waits for a pool again.
8. `Recover`: hold the rail off for the retry delay, then go to `Choose`.
9. `Halted`: hold the rail off until `Message::Resume`, then go to `Choose`.

One select runs every state. It reads STOP and HALT and waits for the state's work.
On HALT, go to `Halted`. On STOP, return from `Miner::run`.
In `Halted`, the select reads only STOP, and `Halted` reads the resume itself.
STOP or HALT during `Step` allows up to 100 ms for the current plan.
In every other state, STOP or HALT ends the state's work immediately.
Either one cuts the rail without a shutdown ramp or a voltage write.
If requests arrive together, STOP takes priority over HALT.
An algorithm change during bring-up takes effect at `StartWork`, without another rail cut.
An algorithm change during `Step` takes effect when the plan completes.

`Choose`, `NoTuner`, `Recover` and `Halted` hold the rail off.
`Reset` and `WaitForPool` also keep the rail off, because `Enable` is the first
state that raises it. The select treats every state except the first four as
powered. When the machine goes from a powered state to one of those four, or STOP
ends it, the select cuts the rail after the state's work ends.
Entry to `NoTuner` or `Halted` also cuts the rail, from any state. The boot cut
only logs a failed disable, and these two states can hold for an unlimited time.
Nothing cancels the cut.
The grace period bounds the wait for the plan, not the complete shutdown.
PSU and reset commands can still wait for hardware access.

The lifecycle status that the API reads follows from the state: `Reset` and
`WaitForPool` report `WaitingForPool`; `Enable` through `StartWork` report
`Starting`; `Step` and `Pause` report `Running`; every state that holds the rail
off reports `Stopped`. The select publishes the status through the mining
`Handle` after the cut, whenever a transition changes it.

The executor clamps rail commands to the board ceiling and frequencies to the chip range.
A failed plan is retried at the next step, planned from the confirmed hardware state.
`PLAN_FAILURE_BUDGET` failures in a row emit a tuner fault, cut the rail, and retry
initialization.
A plan that writes to the hardware clears the count.
A plan that writes nothing, such as one naming a value the chain or the PSU already
holds, leaves it.
STOP and HALT can interrupt the retry delay without another rail cut,
because `Recover` holds the rail off.

A persistent chip-count mismatch parks the affected chain.
The other present chains continue.
If no chain becomes present, the miner cuts the rail and starts another
cycle. It does this for `BRING_UP_CYCLES` cycles. After the last one it
parks the machine, with the chains in reset and the rail off, and waits
for a resume. The manager sends one for a Start that finds nothing
paused, so an operator sends Start, or reboots the board. A cycle that
brings a chain up gives the budget back.

### Policy and hardware boundary

`ubos-tuner` defines the shared `Tuner`, `Readings`, and `Plan` types.
The fixed and power policies stay in `ubos-mining::tuner::fixed` and `ubos-mining::tuner::power_ramp`.
Each returns commands and a delay without hardware access or asynchronous waits.
An empty plan leaves the operating point unchanged and schedules another step.
It does not signal completion to the mining loop.

Each serial actor records its chain's frequency after a successful command.
The PSU task records the voltage a successful PSU command sets.
The plan executor first lowers frequencies, then changes the rail, then raises frequencies.

The firmware names the tuner set as the `T` of `Miner` and the fourth argument of `miner_task!`.
The miniminer boards and the host runner name `Tuners`, which serves both the fixed and the power-ramp policy.
The hashnode and the Forge Axe name `FixedTuner` alone: nobody has measured what they may draw, so they offer no power target.
The loop drives all connectors through `MachineTuner<CHAINS>`.
Each present chain supplies its frequency, chip count, hottest temperature, and smoothed hashrate.
Absent and faulted connectors have `None` entries in `Readings::chains`.
The miner also supplies the PSU snapshot and the last confirmed voltage command.
`Readings::voltage` comes directly from the PSU record.
Telemetry can lag or show voltage droop.

What the platform reports beyond `Readings` arrives as the set's `PlatformReadings`.
The firmware passes the function that reads them as the fifth argument of `miner_task!`.
The miner calls it once per step and passes the result to the tuner, which reads nothing itself.
Both shared policies take `()`, and the four-argument form of the macro supplies it.

The set lists the tuners it offers in `MachineTuner::TUNERS`: each one's name, the `Tuning` variant it reads, and whether a power tuner reads the efficiency.
The firmware passes the list to `HashboardDescriptor::bounds`, so the config accepts only a tuning one of these tuners reads, and the mining form shows only these tuners and the fields they read.
The firmware also passes the input-power range a power target may ask for, which no descriptor states.
The frequency range is the chip's own, and the voltage range runs from zero to the board's rail ceiling.
A new config starts on the `DefaultTuning` the firmware passes to `HashboardDescriptor::mining_info`.
At boot, a stored tuning outside the accepted ranges is clamped into them, and one that no listed tuner reads is replaced by the default tuning; the rest of the stored config is kept.

## Chain presence

A machine can have more hashboard connectors than boards plugged into
them, so enumeration is a per-chain question. `Miner` resets and
enumerates every chain at the same time, each over its own UART, and
records one of four states in that chain's `ChainStats`:

| State | Meaning | What the miner does |
|-------|---------|----------------------|
| `Unknown` | No enumeration attempt has completed yet. | Not a resting state; only seen mid-attempt. |
| `Present` | The chain answered with the board's expected chip count. | The miner tunes this chain. |
| `Absent` | The chain answered zero chips on the last attempt of the retry budget: no hashboard is attached. | Parked in reset; left out of the ramp. |
| `Faulted` | The chain answered a chip count other than expected, refused its safe-boot configuration, or answered no temperature read, on the last attempt of the retry budget. | Parked in reset; left out of the ramp. |

A chain passes only on the chip count its descriptor claims. Every other
answer gets `ENUMERATE_ATTEMPTS` attempts in all, an answer of zero chips
included, in case the failure was a transient chain glitch. A receive
error latched in the enumerate window leaves a populated chain mute for
that attempt, and the empty answer it gives is the answer an empty
connector gives. The budget bounds what the mistake costs either way.
The chain waits `RETRY_SETTLE` between two attempts, because a chip that
answered nothing can answer the attempt that follows a settle. Only a
bring-up where no chain comes up present cuts the rail and starts
another cycle; one absent or faulted chain does not stop the others.

The temperature read is part of the budget because presence is what the
thermal loop measures a chain's silence from. `ChainTemperature` restarts
that clock when a chain turns present, and the per-chip readings are
never cleared, so a chain that reached `Present` with nothing read would
have the loop hold the reading from before the last rail cut for the
whole ramp. Failing the bring-up step instead keeps the rule that a chain
the tuner sees has one reading of its own behind it.

That rule is load-bearing twice over. It also lets `ChainTemperature`
read "no chain present" as an idle board rather than as a fault, because
a present chain always has a reading behind it, so silence from a board
with no present chain is a board with nothing powered.

That reading is what a stop depends on. A rail cut marks every chain
unknown, so a board whose chips are its thermometer falls silent the
moment it stops, and a loop that read that silence as a dead sensor
would hold the board down for the silence its own stop caused. Reading
it as idle instead is what lets the thermal loop release its own
unreadable halt: a halt that waited for a reading could never lift,
because the halt is what ends the readings.

### Tolerance ends at bring-up

That tolerance is a property of enumeration and of nothing after it. A
chain is written as `Present`, `Absent` or `Faulted` only by the
bring-up loop and by the rail cut, so once the machine is mining there
is no path that moves a chain out of the ramp. Two runtime faults
therefore take the whole machine down:

- **A present chain that goes silent.** `ChainTemperature::sample`
  answers `Unreadable` when any present chain has been silent for its
  budget, and the thermal loop halts the board on
  `TemperatureUnreadable`. The other chains are hashing and are stopped
  with it. The sensor cannot do otherwise today: it reads presence and
  does not write it, and an unread die is the greater risk.

  That halt releases itself; see "Two ways out of an overheat halt"
  above. A chain that enumerates clean and goes silent again therefore cycles
  through bring-up about once per silence budget. That is the price of
  the release, and why the release exists is under "Chain presence".
- **A present chain that refuses a ramp step.** The plan executor
  reports the first error, so the miner cuts the rail and retries the
  whole bring-up. A persistent frequency-command failure therefore
  stops the other chains too.

Giving the miner a way to demote a chain after bring-up is the work
this needs, and it is not done.

The PSU rail is shared, but each chain has its own frequency record and plan entry.
Both policies move all present chains toward one frequency.
The fastest chain limits the shared voltage along the `V^2 * f` curve.
The executor ignores plan entries for absent and faulted connectors.

## I2C bus sharing

`firmware/hashnode/src/psu.rs` builds `BoardPsu` over an `I2cDevice`
carved from the shared I2C1 bus and owns the `SIC_EN` (GPIO30) pin. The
I2C1 bus mutex (`Bus`) is owned by `main`, so the temperature sensors
take an additional `I2cDevice` handle from the same mutex. A
`NoopRawMutex` is enough because every I2C consumer (PSU, temperature)
runs on the Core 0 executor; `serial_task` runs on Core 1 but never
touches I2C. If a future I2C consumer moves to Core 1, bump it to
`CriticalSectionRawMutex`.

## UART baud-rate switching

The BM1370 boots at one baud rate and the host bumps it after init. Each
chain's actor owns one full-duplex instance, so `Serial::run` takes a
single UART by `&mut self`, and the board wraps its `BufferedUart` as one
`MinerUart` implementing `serial_baudrate::BaudRate`. The wrapper
delegates to embassy-rp's `BufferedUart::set_baudrate` rather than
poking divisor registers directly; that path runs the full PL011
reconfigure dance (disable, 15-baud settle, restore) a bare register
write would skip. A zero baud rate is rejected once in the actor, since
the setter divides by the rate and would otherwise panic.

## Multiple chains and the core-1 park

A board owns a static array of `Serial`, one per hashboard chain it
wires up; `ubos_metrics::chain` holds one `ChainStats`
per chain in the same order, so each chain's hashrate, share count, and
desync bytes are tracked and published separately. The table has
`ubos_machine::MAX_CHAINS` slots; `ChainStatsHandle::chain_count` says
how many of them the board wired up, and `GET /api/info` carries a
`chains` array with one entry per wired chain, so a single-chain board
reports one entry and never a phantom chain. The dashboard's Hashboards
card renders this array. `Serial::new(chain,
chains)` takes the index of this chain and the board's total chain count, so
sibling chains never hash identical headers. The actor receives the `Park` of
the board in `run`, and it joins that park while it runs.

A flash write that needs exclusive access to the chip UARTs calls
`request` on the board's `Park` and waits for `is_parked`, which is
`true` only once every live chain's actor has parked. Each actor's park
loop yields to the embassy executor on every iteration instead of
busy-spinning, so on a board with more than one chain pinned to the same
core, every actor gets a turn to reach its own park point.

## Logging facade

`ubos-mining`, `ubos-psu`, and `ubos-logging` use `ubos_logging::{info, warn,
error}` rather than `defmt::*!` or `log::*!` directly. The
`ubos-logging` macro facade expands to defmt under firmware feature
flags and to `log` under the host build. The same source compiles for
both targets and the host build installs `env_logger`.

## Fan PWM and tachometers

Code: `firmware/hashnode/src/fan.rs` for the board, and
`ubos/ubos-rp/src/pio_tach.rs` for the tachometer program that the RP
boards share.

Three PWM channels drive the fan headers at 25 kHz, the rate that the
Intel 4-wire fan specification gives. `PWM_SLICE5` channel A is GPIO10
and channel B is GPIO11. `PWM_SLICE9` channel B is GPIO43 on the
`FAN_CONTROL` header. Every channel runs the same duty, so all fans track
together.

An inverter sits between the RP2350 pad and the fan, so `set_pwm` inverts
the commanded duty. A higher requested percent drives a lower GPIO duty.
A duty and RPM sweep on hardware confirmed this.

Five connectors have a tachometer: GPIO24, GPIO25, GPIO23, GPIO38 and
GPIO44. None of these is a channel B of a PWM slice, so the slice edge
counter cannot count them. Each tach drives one PIO state machine, and
the state machine measures the period between two pulses in hardware.

A PIO block sees one window of 32 GPIOs. GPIO38 and GPIO44 sit above
GPIO31, so the tachs split across the two blocks. `PIO0` takes GPIO23,
GPIO24 and GPIO25, and `PIO1` takes GPIO38 and GPIO44. embassy-rp does
not apply the window offset to the `jmp pin` of a state machine, so the
driver corrects that register for `PIO1` by hand. Otherwise the program
on `PIO1` never sees an edge.

The tach RC network charges through a 4.7 kΩ pull-up, so the signal
rises slowly through the input threshold and chatters. The PIO program
accepts an edge only after the line stays high for 31 consecutive
samples, about 31 µs. That window is longer than a chatter pulse and
much shorter than half a real tach period.

The pulses per revolution belong to the board, and the shared module
takes them as a parameter. The hashnode fans give two pulses per
revolution, so `RPM = 60 × pio_hz / (ticks × cycles_per_tick × 2)`.
`BoardTach` drains the latest period for each fan on every control tick
and publishes one labelled `fan_rpm` series per connector. An
empty connector reads 0 rpm.

## Temperature sensors

Code: `firmware/hashnode/src/temperature.rs` binds
`ubos_thermal::SensorArray` to the board's I2C1 bus from the
`ubos_machine::HASHNODE` sensor table.

The hashboard carries a TMP461 sensor and two TMP102 sensors on the
shared I2C1 bus. The TMP461 remote channel reads the ASIC die. The
TMP461 local channel and the two TMP102 sensors read the hashboard.
The halt judges each kind of reading against its own threshold. It
halts when the die reading reaches `dangerous_chip_temp`, or when the
hottest hashboard reading reaches `dangerous_hashboard_temp`. Mining
resumes when every kind of reading is at or below `cooldown_temp`. `BFMX4_HASHBOARD` in `ubos-asic` gives the
factory value of both thresholds.

## Host parity

`host-runner` activates the same backend against a Linux tty (chip
serial) and `/dev/i2c-N` (PSU) via `UBOS_I2C_BUS`. Without the env var
the runner skips the spawn so the rest of the tasks (network, stratum,
metrics, HTTP) still come up when no hashboard is wired in.
