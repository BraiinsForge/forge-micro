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
| `miner_task` | `mining.rs` | Drives `Miner::new(psu, BOARD, chains, resets).run()` against the tuner selected by `MinerConfig::algorithm` (`FixedTuner` by default; see `TuningAlgorithm`). Bring-up / teardown state machine: PSU up, reset sequence, per-chain enumeration, tuner init, mine, tuner deinit, PSU off. |
| `power_stats_task` | `psu.rs` | Drives `ubos_psu::run_power_stats`. Polls the SIC45x input/output power every `POWER_STATS_INTERVAL` (two seconds) and publishes them. |

The SIC45x rail is gated by the `SIC_EN` GPIO the `BoardPsu` owns, not by
a PMBus register, so there is no separate PSU-watchdog task; only
`miner_task` raises `SIC_EN`, during bring-up, and drops it when mining
halts.

`serial_task` and `miner_task` never return under normal operation.
`Miner::run` waits on either `STOP_MINING` (orderly shutdown path; runs
`tuner.deinit` before powering down) or a `Message::Halt` edge from the
manager (thermal trip, fan fault, or operator pause; cuts the rail
immediately without deinit). After a halt, the loop blocks in
`wait_halt_clear_or_stop()` until either a `Message::Resume` arrives and
bring-up is retried, or `STOP_MINING` fires and the task tears down. On
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
| `ubos-cooling`   | `BrokenFan`                   | `BrokenFan`                |
| stratum client   | `PoolUnreachable`             | `PoolUnreachable`          |
| tuner            | `TunerFault`                  | `TunerFault`               |
| OTA (`ubos-api`) | `OtaBegin`                    | `OtaUpdate`                |
| operator         | `Stop`                        | `UserRequest`              |

Domain producers raise typed messages; they never name a `PauseReason`.
Manager translates and serialises the halt/resume sequence so mining
sees one logical halted-state regardless of how many pause reasons are
active concurrently.

`ubos_mining::miner::next_halt_change()` reads `Message::Halt` /
`Message::Resume` off the mining command channel; those are the only two
variants. Frequency, voltage, and algorithm edits do not travel the
command channel: `Miner::run` subscribes to `ubos_config::handle()`
directly and reads the live snapshot at the top of each bring-up. An
algorithm change tears down the active tuner (`tuner.deinit` + rail cut)
and rebuilds it; other tuning edits apply on the next bring-up.

## Mining state machine

One `Miner` owns the PSU rail and every chain of the machine.

`Miner::run` first disables the rail once, before the first reset. This
puts every board in a known-off state before its first bring-up, no matter
what the last boot cycle left behind.

Then `Miner::run` repeats:

1. Read the live `ubos-config` snapshot and select the tuner by
   `miner.algorithm`.
2. Bring the rail up via `BoardPsu` (raises `SIC_EN` once the SIC45x
   answers a status read).
3. Sequence every chain's reset pin together (low to assert, high to
   release).
4. Reset and enumerate every chain at once. See "Chain presence" below.
5. Run `tuner.init` against every chain the enumeration step found
   present (cancellable by `STOP_MINING` or `Message::Halt`).
6. Mine until `STOP_MINING` fires, a `Message::Halt` edge arrives, or the
   configured algorithm changes.
7. On `Message::Halt`: cut the rail immediately, then block on
   `wait_halt_clear_or_stop` until `Message::Resume`.
8. On an algorithm change: run `tuner.deinit`, cut the rail, and rebuild
   the tuner on the next iteration.
9. On `STOP_MINING`: run `tuner.deinit` and drop the rail.

A fatal init failure -- chip silent at the new line rate, register
mismatch, etc. -- cuts the rail, sleeps, and restarts. The chip is never
left powered without supervision.

## Chain presence

A machine can have more hashboard connectors than boards plugged into
them, so enumeration is a per-chain question. `Miner` resets and
enumerates every chain at the same time, each over its own UART, and
records one of four states in that chain's `ChainStats`:

| State | Meaning | What the miner does |
|-------|---------|----------------------|
| `Unknown` | No enumeration attempt has completed yet. | Not a resting state; only seen mid-attempt. |
| `Present` | The chain answered with the board's expected chip count. | The tuner ramps this chain. |
| `Absent` | The connector answered with zero chips: no hashboard is attached. | Parked in reset; left out of the ramp. |
| `Faulted` | The chain answered a chip count other than expected on every attempt up to the retry budget. | Parked in reset; left out of the ramp. |

An empty connector is parked on the first attempt with no retry: retrying
it on every bring-up would cost the machine its bring-up time for
nothing. A chip-count mismatch gets `ENUMERATE_ATTEMPTS` retries before
it is parked, in case the mismatch was a transient chain glitch. Only a
bring-up where no chain comes up present cuts the rail and retries the
whole machine; one absent or faulted chain does not stop the others.

Voltage and frequency are one value for the whole machine today: the
PSU rail is shared, and the tuner steps every present chain's frequency
in lockstep so the rail sees one load profile instead of one chain
climbing at a time. The tuner reaches a chain only through the
`ChainControl` handle `MinerControl::present_chains` yields, never
through an index, so it can never address a chain the miner did not
find present.

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
wires up; `ubos_mining::chain_stats::CHAIN_STATS` holds one `ChainStats`
per chain in the same order, so each chain's hashrate, share count, and
desync bytes are tracked and published separately. `Serial::new(chain,
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
