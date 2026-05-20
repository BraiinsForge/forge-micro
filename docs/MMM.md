# MMM backend (BM1362 hashboard)

`firmware/bmm-pico` built with `--features backend-mmm` drives a real
BM1362 hashboard instead of mining in software or talking to the
`dummy-asic` peer. The driver lives in the workspace-shared `mmm/` crate.
This page covers how the BMM Adapter wires the driver up.

## Topology

```
+---------------------+        UART1 (BM1362 serial)        +-------------+
| firmware/bmm-pico   |<----------------------------------->| BMM100      |
|                     |        I2C1 (BMM100 PSU)            | hashboard   |
| serial_task <-------|<----------------------------------->| (BM1362     |
| miner_task          |        GPIO (reset)                 |  chip chain)|
| watchdog_task       |<----------------------------------->|             |
| power_stats_task    |                                     +-------------+
+---------------------+
```

The same backend code (the `mmm` and `ubos-psu` crates) runs on the
`host-runner` against a Linux tty and `/dev/i2c-N`, which is how the
driver is exercised without burning out hashboards on the bench.

## Long-running tasks

`firmware/bmm-pico/src/mining.rs` spawns two tasks; `firmware/bmm-pico/src/psu.rs`
adds two more:

| Task | Owner | Role |
|------|-------|------|
| `serial_task` | `mining.rs` | Drives `mmm::serial::SERIAL.run(rx, tx)`. Single actor for chip-side serial: command dispatch, parser, in-flight work registry, baud-rate switch. |
| `miner_task` | `mining.rs` | Drives `mmm::miner::run(psu, reset)`. Bring-up / teardown state machine: PSU up, reset sequence, init replay, mine, deinit, PSU off. |
| `watchdog_task` | `psu.rs` | Drives `ubos_psu::run_watchdog`. Keeps the BMM100 in its active state by re-asserting the protection-defeat sequence. |
| `power_stats_task` | `psu.rs` | Polls the INA230 input/output power readings every two seconds and logs them. Same cadence as the host-runner so prometheus rates are comparable. |

`serial_task` and `miner_task` never return under normal operation. There
is no SIGINT on bare metal, so `STOP_MINING` is never raised and
`mmm::miner::run` parks in its bring-up loop. If either task's main loop
ever does return, the task idles for 60 s between log lines so the rest
of the firmware (network, metrics, watchdog) keeps running and logs can
be pulled.

## Mining state machine

`mmm::miner::run` repeats:

1. Bring the BMM100 rails up via `ubos_psu::Psu`.
2. Sequence the BM1362 reset pin (low to assert, high to release).
3. Replay the canned init trace against the chip chain.
4. Mine until `STOP_MINING` fires (never, on bare metal).
5. Replay the deinit trace.
6. Power down the BMM100.

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
embassy-rp 0.10 exposes runtime baud-rate switching only on the unsplit
`BufferedUart` via `&mut self`; the per-half `info: &'static Info` that
drives `set_baudrate_inner` is private. `mmm::serial::Serial::run` takes
two independent halves, each implementing `serial_baudrate::BaudRate`
with `&self`. The `firmware/bmm-pico/src/uart_baudrate.rs` wrapper
re-implements the PL011 divisor math against the PAC, mirroring
`embassy_rp::uart::Uart::set_baudrate_inner`, so each half can retune the
line. The PL011 `ibrd` field is 16 bits, so the integer divisor
saturates at 65 535 like the reference implementation.

## Logging facade

`mmm`, `ubos-psu`, and `ubos-logging` use `ubos_logging::{info, warn,
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

Code: `firmware/bmm-pico/src/temperature.rs`.

The BMM101 hashboard carries four LM75A sensors at I2C addresses
`0x49`-`0x4c` (IC12D, IC12C, IC12B, IC12A) on the shared I2C1 bus. Each
raw reading is offset by `+7 °C` (`OFFSET_MILLICELSIUS = 7000`) to
approximate chip junction temperature from the board-mounted sensor; this
matches the `BRAIINS_MINI_BMM101_BM1362_4X1` descriptor in bos-main. The
controller exposes the maximum across all four sensors so that a single hot
spot drives the thermal limit.

## Host parity

`host-runner` activates the same backend via `MMM_I2C_BUS=/dev/i2c-N`
(PSU) and `/dev/ttySTM1` (chip serial). Without the env var the runner
skips the spawn so the rest of the tasks (network, stratum, metrics,
HTTP) still come up when no hashboard is wired in.
