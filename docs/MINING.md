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
| `serial_task` | `mining.rs` | Drives `ubos_mining::serial::SERIAL.run(...)`. Single actor for chip-side serial: command dispatch, parser, in-flight work registry, baud-rate switch. Pinned to Core 1 so embassy-net on Core 0 stays responsive. |
| `miner_task` | `mining.rs` | Drives `Miner::new(psu, reset, BOARD).run()` against the tuner selected by `MinerConfig::algorithm` (`FixedTuner` by default; see `TuningAlgorithm`). Bring-up / teardown state machine: PSU up, reset sequence, tuner init, mine, tuner deinit, PSU off. |
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

`Miner::run` repeats:

1. Read the live `ubos-config` snapshot and select the tuner by
   `miner.algorithm`.
2. Bring the rail up via `BoardPsu` (raises `SIC_EN` once the SIC45x
   answers a status read).
3. Sequence the BM1370 reset pin (low to assert, high to release).
4. Run `tuner.init` against the chip chain (cancellable by `STOP_MINING`
   or `Message::Halt`).
5. Mine until `STOP_MINING` fires, a `Message::Halt` edge arrives, or the
   configured algorithm changes.
6. On `Message::Halt`: cut the rail immediately, then block on
   `wait_halt_clear_or_stop` until `Message::Resume`.
7. On an algorithm change: run `tuner.deinit`, cut the rail, and rebuild
   the tuner on the next iteration.
8. On `STOP_MINING`: run `tuner.deinit` and drop the rail.

A fatal init failure -- chip silent at the new line rate, register
mismatch, etc. -- cuts the rail, sleeps, and restarts. The chip is never
left powered without supervision.

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

The BM1370 boots at one baud rate and the host bumps it after init. The
actor owns one full-duplex instance, so `ubos_mining::serial::Serial::run`
takes a single UART by `&mut self`, and the board wraps its
`BufferedUart` as one `MinerUart` implementing `serial_baudrate::BaudRate`.
The wrapper delegates to embassy-rp's `BufferedUart::set_baudrate` rather
than poking divisor registers directly; that path runs the full PL011
reconfigure dance (disable, 15-baud settle, restore) a bare register
write would skip. A zero baud rate is rejected once in the actor, since
the setter divides by the rate and would otherwise panic.

## Logging facade

`ubos-mining`, `ubos-psu`, and `ubos-logging` use `ubos_logging::{info, warn,
error}` rather than `defmt::*!` or `log::*!` directly. The
`ubos-logging` macro facade expands to defmt under firmware feature
flags and to `log` under the host build. The same source compiles for
both targets and the host build installs `env_logger`.

## Fan PWM and tachometers

Code: `firmware/hashnode/src/fan.rs` for the PWM, `ubos/ubos-rp/src/pio_tach.rs`
for the tachometer program the boards share.

`PWM_SLICE5` drives both channel A (GPIO10) and channel B (GPIO11) at
25 kHz, the Intel 4-wire fan spec rate. Both channels run the same duty
so all three fan connectors track together. An inverting buffer sits
between the RP2350 pad and the fan, so `set_pwm` inverts the commanded
duty: a higher requested percent drives a lower GPIO duty, confirmed on
hardware by a duty/RPM sweep.

Each connector has its own tachometer (GPIO24, GPIO25, GPIO23). None map
to a `PWM` slice channel B, so the slice edge counter is unusable; each
tach instead drives one `PIO0` state machine that times the pulse-to-
pulse period in hardware. The tach RC network's slow rising edge chatters
around the input threshold, so the PIO program only accepts an edge once
the line has stayed high for 31 consecutive samples (~31 µs), longer than
a chatter pulse yet far shorter than a real tach half-period. Standard
4-wire fans emit two pulses per revolution, so
`RPM = 60 × pio_hz / (ticks × cycles_per_tick × 2)`. `BoardTach` drains
the latest period per fan each control tick and publishes one labelled
`board_fan_rpm` series per connector.

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
