# ubos-mining backend (BM1362 hashboard)

Every firmware crate drives a real BM1362 hashboard via the
workspace-shared `ubos/ubos-mining/` crate. This page covers how the BMM
Adapter wires the driver up.

## Topology

```
+---------------------+        UART1 (BM1362 serial)        +-------------+
| firmware/bmm-pico   |<----------------------------------->| BMM101      |
|                     |        I2C1 (BMM101 PSU)            | hashboard   |
| serial_task <-------|<----------------------------------->| (BM1362     |
| miner_task          |        GPIO (reset)                 |  chip chain)|
| watchdog_task       |<----------------------------------->|             |
| power_stats_task    |                                     +-------------+
+---------------------+
```

The same backend code (the `ubos-mining` and `ubos-psu` crates) runs on the
`host-runner` against a Linux tty and `/dev/i2c-N`, which is how the
driver is exercised without burning out hashboards on the bench.

## Long-running tasks

`firmware/bmm-pico/src/mining.rs` spawns two tasks; `firmware/bmm-pico/src/psu.rs`
adds two more:

| Task | Owner | Role |
|------|-------|------|
| `serial_task` | `mining.rs` | Drives `ubos_mining::serial::SERIAL.run(rx, tx)`. Single actor for chip-side serial: command dispatch, parser, in-flight work registry, baud-rate switch. |
| `miner_task` | `mining.rs` | Drives `Miner::new(psu, reset).run()` against the tuner selected by `MinerConfig::algorithm` (`FixedTuner` by default; see `TuningAlgorithm`). Bring-up / teardown state machine: PSU up, reset sequence, tuner init, mine, tuner deinit, PSU off. |
| `watchdog_task` | `psu.rs` | Drives `ubos_psu::run_watchdog`. Keeps the BMM101 in its active state by re-asserting the protection-defeat sequence. |
| `power_stats_task` | `psu.rs` | Polls the INA230 input/output power readings every two seconds and logs them. Same cadence as the host-runner so prometheus rates are comparable. |

`serial_task` and `miner_task` never return under normal operation.
`Miner::run` waits on either `STOP_MINING` (orderly shutdown path; runs
`tuner.deinit` before powering down) or a `Message::Halt` edge from the
manager (thermal trip, fan fault, or operator pause; cuts the rail
immediately without deinit). After a halt, the loop blocks in
`wait_halt_clear_or_stop()` until either a `Message::Resume` arrives and
bring-up is retried, or `STOP_MINING` fires and the task tears down. If
either task's main loop ever does return, the task idles for 60 s between
log lines so the rest of the firmware (network, metrics, watchdog) keeps
running and logs can be pulled.

## Halt/resume edges via `ubos-manager`

`ubos-manager::run_manager` is the sole caller of `MiningHandle::halt`
and `MiningHandle::resume`. It owns a `PauseReasons` bitmask published
on `STATE_WATCH` and flips edges on the empty/non-empty boundary:

| Producer        | `Message` raised on manager channel | Bit asserted               |
|-----------------|-------------------------------------|----------------------------|
| `ubos-thermal`  | `DangerousTemperature`              | `DangerousTemperature`     |
| cooling/fan     | `BrokenFan`                         | `BrokenFan`                |
| stratum client  | `PoolUnreachable`                   | `PoolUnreachable`          |
| tuner           | `TunerFault`                        | `TunerFault`               |
| operator        | `Stop`                              | `UserRequest`              |

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
2. Bring the BMM101 rails up via `ubos_psu::Psu`.
3. Sequence the BM1362 reset pin (low to assert, high to release).
4. Run `tuner.init` against the chip chain (cancellable by `STOP_MINING`
   or `Message::Halt`).
5. Mine until `STOP_MINING` fires, a `Message::Halt` edge arrives, or the
   configured algorithm changes.
6. On `Message::Halt`: cut the rail immediately, then block on
   `wait_halt_clear_or_stop` until `Message::Resume`.
7. On an algorithm change: run `tuner.deinit`, cut the rail, and rebuild
   the tuner on the next iteration.
8. On `STOP_MINING`: run `tuner.deinit` and power down the BMM101.

A fatal init failure -- chip silent at the new line rate, register
mismatch, etc. -- cuts the rail, sleeps, and restarts. The chip is never
left powered without supervision.

## I2C bus sharing

`firmware/bmm-pico/src/psu.rs` owns the static cells for the PSU state
and handle. The I2C1 bus mutex (`Bus`) is owned by `main` so any future
consumer can take an additional `I2cDevice` handle from the same mutex
without going through `psu.rs`. A `NoopRawMutex` is enough as long as
every I2C consumer runs on the same Core 0 executor; if a future task
moves to Core 1, bump it to `CriticalSectionRawMutex`.

## UART baud-rate switching

The BM1362 boots at one baud rate and the host bumps it after init.
The actor owns one full-duplex instance, so
`ubos_mining::serial::Serial::run` takes a single UART by `&mut self`
and each board wraps its UART as one `MinerUart` implementing
`serial_baudrate::BaudRate`. Each wrapper delegates to the HAL's own
baud setter rather than poking divisor registers directly:
`BufferedUart::set_baudrate` on the embassy-rp boards (bmm-pico,
rp2040, rp2350, hashnode), `UartTx::set_baudrate` on embassy-stm32
(stm32f407, stm32f439), and `Uart::apply_config` on esp-hal, which
must hold the unsplit `Uart` because esp-hal exposes no baud setter on
the split halves. The HAL paths run the full reconfigure dance the
bare register write skipped: embassy-rp the PL011 disable and
15-baud settle, embassy-stm32 the OVER8 auto-select from the captured
kernel clock, esp-hal the `reg_update` pulse. A zero baud rate is
rejected once in the actor, since the embassy-rp and embassy-stm32
setters divide by the rate and would otherwise panic.

## Logging facade

`ubos-mining`, `ubos-psu`, and `ubos-logging` use `ubos_logging::{info, warn,
error}` rather than `defmt::*!` or `log::*!` directly. The
`ubos-logging` macro facade expands to defmt under firmware feature
flags and to `log` under the host build. The same source compiles for
both targets and the host build installs `env_logger`.

## Fan PWM/tach pin swap

Code: `firmware/bmm-pico/src/fan.rs`.

The BMM Adapter schematic names the J13 connector signals `FAN_PWM` (pin 3)
and `FAN_RPM` (pin 4), but the Intel 4-wire fan spec assigns PWM to pin 4
and TACH to pin 3. The firmware follows the spec, so a swap cable is
required between the J13 connector and the fan: pins 3 and 4 must be
crossed relative to the schematic net names.

The tach output is open-collector; the RP2350 pad's internal pull-up is
enabled to idle the line high between pulses. Standard 4-wire fans emit two
pulses per revolution, so `RPM = pulses × 60 / 2` over the sample window.

## Temperature sensors

Code: `ubos/ubos-temperature/src/lm75a.rs` (four-sensor logic);
`firmware/bmm-pico/src/temperature.rs` binds it to the board's I2C bus.

The BMM101 hashboard carries four LM75A sensors at I2C addresses
`0x49`-`0x4c` (IC12D, IC12C, IC12B, IC12A) on the shared I2C1 bus. Each
raw reading is offset by `+7 °C` (`OFFSET_MILLICELSIUS = 7000`) to
approximate chip junction temperature from the board-mounted sensor.
The controller exposes the maximum across all four sensors so that a
single hot spot drives the thermal limit.

## Host parity

`host-runner` activates the same backend via `UBOS_I2C_BUS=/dev/i2c-N`
(PSU) and `/dev/ttySTM1` (chip serial). Without the env var the runner
skips the spawn so the rest of the tasks (network, stratum, metrics,
HTTP) still come up when no hashboard is wired in.
