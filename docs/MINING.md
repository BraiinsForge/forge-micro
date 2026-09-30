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
| power_stats_task     |<------------------------------------->|              |
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
| `miner_task` | `mining.rs` | Drives `Miner::new(psu, BOARD, chains, resets).run()` against the tuner selected by `MinerConfig::algorithm` (`FixedTuner` by default; see `TuningAlgorithm`). Bring-up / teardown state machine: reset assert, PSU up, reset release, per-chain enumeration, per-chain safe-boot configuration, start work, apply tuner plans, PSU off. |
| `power_stats_task` | `psu.rs` | Drives `ubos_psu::run_power_stats`. Polls the SIC45x input/output power every `POWER_STATS_INTERVAL` (two seconds) and publishes them. |

The SIC45x rail is gated by the `SIC_EN` GPIO the `BoardPsu` owns, not by
a PMBus register, so there is no separate PSU-watchdog task; only
`miner_task` raises `SIC_EN`, during bring-up, and drops it when mining
halts.

`serial_task` and `miner_task` never return under normal operation.
`Miner::run` responds to `STOP_MINING` with a rail cut.
HALT, STOP, and algorithm changes interrupt plans.
The miner allows up to 100 ms for an interrupted plan to complete.
The miner keeps the first interruption and skips the remaining plan delay.
All three interruptions then cut the rail without a shutdown ramp or voltage write.
After HALT, the loop waits for `Message::Resume` or `STOP_MINING`.
Resume restarts bring-up, while STOP ends the loop. On
bare metal there is no `STOP_MINING`, so if either loop ever returns the
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
- `config::BOARD` (`&HashboardDescriptor`) carries the chip-neutral
  hashboard facts the actor and tuner need.

## Halt/resume edges via `ubos-manager`

`ubos-manager::run_manager` is the sole caller of `MiningHandle::halt`
and `MiningHandle::resume`. It owns a `PauseReasons` bitmask published
on `STATE_WATCH` and flips edges on the empty/non-empty boundary:

| Producer         | `Message` sent to the manager | `PauseReason` bit asserted |
|------------------|-------------------------------|----------------------------|
| `ubos-thermal`   | `DangerousTemperature`        | `DangerousTemperature`     |
| `ubos-thermal`   | `TemperatureUnreadable`       | `TemperatureUnreadable`    |
| `ubos-cooling`   | `BrokenFan`                   | `BrokenFan`                |
| stratum client   | `PoolUnreachable`             | `PoolUnreachable`          |
| tuner            | `TunerFault`                  | `TunerFault`               |
| OTA (`ubos-api`) | `OtaBegin`                    | `OtaUpdate`                |
| operator         | `Stop`                        | `UserRequest`              |

### Two ways out of an overheat halt

Where a reading survives the rail cut, the thermal loop sees the
cooldown itself and sends `TemperatureNormal`. Where the chips
themselves are the thermometer, the trip cuts the rail those chips
answer on: the board cools where nothing can see it, no reading can
report the cooldown, and only the operator's `TemperatureAcknowledged`
message clears the halt.

The unreadable halt is different from both: the loop retracts it
itself, once a reading returns or the board goes idle.

Domain producers raise typed messages; they never name a `PauseReason`.
Manager translates and serialises the halt/resume sequence so mining
sees one logical halted-state regardless of how many pause reasons are
active concurrently.

The mining command channel carries `Message::Halt` and `Message::Resume`.
The active miner ignores redundant Resume messages.
After Halt, the miner waits for Resume or `STOP_MINING`.
A Resume queued after Halt remains available to that wait.

Frequency, voltage, and algorithm edits do not travel the command channel.
`Miner::run` subscribes to `ubos_config::handle()` directly.
The miner reads the latest frequency and voltage target before each policy step.
These edits do not restart mining or interrupt initialization.
An algorithm change restarts mining with a new policy after initialization completes.
Other configuration changes do not restart mining.

## Mining state machine

One `Miner` owns the PSU rail and every chain of the machine.

`Miner::run` first attempts to disable the rail before the first reset.
If the PSU-disable command fails, it logs the error and continues with bring-up.

Then `Miner::run` repeats:

1. Read the configuration and select the tuner by `miner.algorithm`.
2. Assert reset on every chain, enable the PSU, and release reset with the existing settle delays.
3. Reset, enumerate, and configure all chains concurrently.
   Retry failed attempts, then park absent or faulted chains in reset.
   Read chip temperatures before a chain becomes present.
4. Set the open-core voltage and start work on every present chain.
5. Read the latest target, feed readings to the tuner, bound its plan, and apply it.
   Wait for the plan delay before the next step.
6. On HALT, STOP, or an algorithm change, allow up to 100 ms for the current plan.
   Skip the remaining plan delay and cut the rail without a shutdown ramp.
7. After HALT, wait for `Message::Resume` or `STOP_MINING`.
   After an algorithm change, restart initialization with the new policy.
   After STOP, return from the mining loop.

STOP or HALT during initialization cuts the rail immediately.
The miner retains an accepted interruption until cleanup completes.
Configuration publications cannot cancel that cleanup.
If requests arrive together, STOP takes priority over HALT, then an algorithm change.
The grace period bounds the wait for the plan, not the complete shutdown.
PSU and reset commands can still wait for hardware access.

The executor clamps rail commands to the board ceiling and frequencies to the chip range.
A failed plan emits a tuner fault, cuts the rail, and retries initialization.
STOP and HALT can interrupt the retry delay without another rail cut.

A persistent chip-count mismatch parks the affected chain.
The other present chains continue.
If no chain becomes present, the miner cuts the rail and retries the machine.

### Policy and hardware boundary

`ubos-tuner` defines the shared `Tuner`, `Readings`, and `Plan` types.
The fixed policy stays in `ubos-mining::tuner::fixed`.
It returns commands and a delay without hardware access or asynchronous waits.
An empty plan leaves the operating point unchanged and schedules another step.
It does not signal completion to the mining loop.

Each serial actor records its chain's frequency after a successful command.
The PSU driver records the shared voltage after a successful command.
The plan executor first lowers frequencies, then changes the rail, then raises frequencies.

The loop drives all connectors through `Tuner<CHAINS>`.
Each present chain supplies its frequency, chip count, hottest temperature, and smoothed hashrate.
Absent and faulted connectors have `None` entries in `Readings::chains`.
The miner also supplies the PSU snapshot and the last confirmed voltage command.
`Readings::voltage` comes directly from the PSU record.
Telemetry can lag or show voltage droop.

The runner supplies experiment results through `PlatformReadings`, which is empty in the fixed policy.

## Chain presence

A machine can have more hashboard connectors than boards plugged into
them, so enumeration is a per-chain question. `Miner` resets and
enumerates every chain at the same time, each over its own UART, and
records one of four states in that chain's `ChainStats`:

| State | Meaning | What the miner does |
|-------|---------|----------------------|
| `Unknown` | No enumeration attempt has completed yet. | Not a resting state; only seen mid-attempt. |
| `Present` | The chain answered with the board's expected chip count. | The miner tunes this chain. |
| `Absent` | The connector answered with zero chips: no hashboard is attached. | Parked in reset; left out of the ramp. |
| `Faulted` | The chain answered a chip count other than expected, refused its safe-boot configuration, or answered no temperature read, on every attempt up to the retry budget. | Parked in reset; left out of the ramp. |

An empty connector is parked on the first attempt with no retry: retrying
it on every bring-up would cost the machine its bring-up time for
nothing. A chip-count mismatch, a refused configuration, or a silent
temperature read gets `ENUMERATE_ATTEMPTS` attempts in all before it is
parked, in case the failure was a transient chain glitch. Only a bring-up
where no chain comes up present cuts the rail and retries the whole
machine; one absent or faulted chain does not stop the others.

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
The fixed policy moves all present chains toward one target.
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
chains)` takes this chain's index and the board's total chain count so
sibling chains never hash identical headers.

A flash write that needs exclusive access to the chip UARTs calls
`request_core1_park()` and waits for `core1_is_parked()`, which is
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
and publishes one labelled `board_fan_rpm` series per connector. An
empty connector reads 0 rpm.

## Temperature sensors

Code: `firmware/hashnode/src/temperature.rs` binds
`ubos_thermal::SensorArray` to the board's I2C1 bus from the
`ubos_machine::HASHNODE` sensor table.

The hashboard carries a TMP461 remote-diode sensor reading the ASIC die
plus two TMP102 PCB sensors on the shared I2C1 bus. The controller
exposes the maximum across the sensors so a single hot spot drives the
thermal limit. Because the safety gate reads the real die temperature
from the TMP461 remote channel, the Hashnode dangerous-temperature
threshold is raised (toward ~108 °C) in the per-board `ThermalConfig`
rather than the shared 90 °C PCB-calibrated default.

## Host parity

`host-runner` activates the same backend against a Linux tty (chip
serial) and `/dev/i2c-N` (PSU) via `UBOS_I2C_BUS`. Without the env var
the runner skips the spawn so the rest of the tasks (network, stratum,
metrics, HTTP) still come up when no hashboard is wired in.
