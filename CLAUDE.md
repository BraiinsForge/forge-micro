# ubos-main

## Build

Each crate must be built from its own directory to pick up the
per-crate `.cargo/config.toml` target. Building from the workspace
root selects the wrong target and fails.

All builds must go through `nix develop` shells. The `arm` shell
covers ARM targets; the `esp` shell covers the Xtensa target.

```sh
nix develop .#arm -c bash -c 'cd firmware/bmm-pico    && cargo check'
nix develop .#arm -c bash -c 'cd firmware/rp2040      && cargo check'
nix develop .#arm -c bash -c 'cd firmware/rp2350      && cargo check'
nix develop .#arm -c bash -c 'cd firmware/stm32f407   && cargo check'
nix develop .#arm -c bash -c 'cd firmware/stm32f439   && cargo check'
nix develop .#esp -c bash -c 'cd firmware/esp32s3 && cargo check'
```

## Flash + monitor

Each firmware crate's `.cargo/config.toml` pins the probe serial,
so `cargo run --release` flashes the right board without any
`--probe` flag. Run from inside the crate directory.

```sh
nix develop .#arm -c bash -c 'cd firmware/bmm-pico  && cargo run --release'
nix develop .#arm -c bash -c 'cd firmware/rp2040    && cargo run --release'
nix develop .#arm -c bash -c 'cd firmware/rp2350    && cargo run --release'
nix develop .#arm -c bash -c 'cd firmware/stm32f407 && cargo run --release'
nix develop .#arm -c bash -c 'cd firmware/stm32f439 && cargo run --release'
nix develop .#esp -c bash -c 'cd firmware/esp32s3   && cargo run --release'
```

Every board drives a BM1362 hashboard via MMM unconditionally. The
`display` feature is always in defaults and SV2 is always built on all
boards.

The bmm-pico crate also has multiple boards attached (six). Aliases
`cargo run-1` through `cargo run-6` pin the probe serial of each.
The bare `cargo run --release` auto-picks when only one board is
plugged in. Run from `firmware/bmm-pico/` under `.#arm`:

```sh
nix develop .#arm -c bash -c 'cd firmware/bmm-pico && cargo run-3 --release'
```

`probe-rs run` flashes then streams defmt logs. To flash and detach,
send SIGINT (Ctrl+C, or `kill -INT <pid>`) to the runner — `cargo` or
`probe-rs-runner` — after the boot banner; the firmware keeps running.
Do not SIGTERM `probe-rs` itself: it is the client of the remote
`probe-rs-server` on the NUC (`PROBE_RS_REMOTE_HOST`), and a hard kill
can leave the remote session in a bad state.

If `cargo run` returns silently (no defmt output, board uptime keeps
growing in prometheus), the CMSIS-DAP probe is wedged per
probe-rs/probe-rs#1849. The runner now defaults `RUST_LOG=info`,
which surfaces "Could not determine a suitable packet size for this
probe" instead of silent exit. Recovery: physically unplug and
replug the affected Pico-debugprobe (the Realtek 0bda:5411 hub on
the NUC reports `ppps` but does not actually cut power, so
`uhubctl` and software USB resets do not help). `probe-rs info` is
what triggers the wedge; never run it twice in a row on the same
probe.

## Targets

| Crate | Target | MCU |
|-------|--------|-----|
| bmm-pico | thumbv8m.main-none-eabihf | RP2350A (Cortex-M33) |
| rp2040 | thumbv6m-none-eabi | RP2040 (Cortex-M0+) |
| rp2350 | thumbv8m.main-none-eabihf | RP2350A (Cortex-M33) |
| stm32f407 | thumbv7em-none-eabihf | STM32F407VGT6 (Cortex-M4F) |
| stm32f439 | thumbv7em-none-eabihf | STM32F439ZIT6 (Cortex-M4F) |
| esp32s3 | xtensa-esp32s3-none-elf | ESP32-S3 (Xtensa LX7) |

## Conventions

- All crates use `portable-atomic` for a uniform atomics API across targets.
- The cortex-m firmware (rp2040, rp2350, stm32f407, stm32f439) uses
  `#[cortex_m_rt::entry]` and drives `embassy_executor::raw::Executor`
  via `ubos_metrics::cpu_load::run_cortex_m_executor` so the poll/wfe
  loop can bracket `wfe` with `Instant::now()` reads. bmm-pico stays
  on `#[embassy_executor::main]` because its core-1 split runs a
  separate hand-managed executor for the MMM serial worker; the
  busy-ratio metric is intentionally absent there.
- The esp32s3 firmware uses `#[esp_hal::main]` and constructs the
  esp-rtos embassy executor manually so it can pass idle hooks to
  `Executor::run_with_callbacks`.
- All boards share network tasks, constants, buffers, and response
  building via the `ubos-core`, `ubos-net`, and `ubos-metrics` crates
  under `ubos/`.
- Embassy task constructors return `Result<SpawnToken, SpawnError>`;
  `spawner.spawn()` returns `()`. The `unwrap!` wraps the constructor.
- ESP32 crate pins `embassy-executor = "0.9.1"` to match `esp-rtos 0.2.0`
  internal dependency (workspace `0.10.0` is incompatible).
- UART for the BM1362 chip serial link uses DMA on cortex-m targets:
  `embassy-stm32` `RingBufferedUartRx` for stm32f407/stm32f439, and
  `embassy-rp` `BufferedUart` (interrupt-driven, 1 KiB software ring
  buffer each direction) for rp2040/rp2350/bmm-pico. The RP hardware
  FIFO is 32 bytes; one-shot DMA would overflow it between `read()`
  calls at 921600 baud, and embassy-rp 0.10.0 ships no ring-buffered
  DMA RX. `BufferedUart` already implements `embedded_io_async::Read`
  and `Write` directly, so no newtype adapter is needed. The esp32s3
  board firmware stays on the interrupt+FIFO `Uart::into_async()` path
  because esp-hal 1.0.0 ships no UART DMA driver. The closed-after-1.0.0 issues
  esp-rs/esp-hal#2642 (UART DMA support) and #4720 (circular UHCI RX)
  track this gap; revisit when the next esp-hal release reaches this
  workspace. The S3's 128-byte hardware FIFO plus its dual-core split
  absorb the ETH-ISR-class jitter that overran the 1-byte USART
  holding register on stm32f407.
