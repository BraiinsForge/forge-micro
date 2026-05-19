# Ethernet Examples

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

Default features vary by board: `rp2040`, `rp2350`, `stm32f407`,
and `esp32s3` enable `["display", "backend-uart"]`; `bmm-pico`
enables `["display", "backend-mmm"]`; `stm32f439` ships with no defaults so the
dummy-asic peer can be attached separately. Switch to CPU mining
with `--no-default-features --features "display,backend-cpu"` on
the boards with displays, or `--features backend-cpu` on stm32f439.
The `backend-cpu` and `backend-uart` features are mutually exclusive.

The dummy-asic crate has multiple peers attached, so it uses
per-host run aliases that override the runner with the right probe
serial. Run from `dummy-asic/` under `.#esp`:

```sh
nix develop .#esp -c bash -c 'cd dummy-asic && cargo run-rp2040    --release'
nix develop .#esp -c bash -c 'cd dummy-asic && cargo run-rp2350    --release'
nix develop .#esp -c bash -c 'cd dummy-asic && cargo run-stm32f407 --release'
nix develop .#esp -c bash -c 'cd dummy-asic && cargo run-esp32s3   --release'
# stm32f439 alias still has a TODO probe serial.
```

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

The dummy-asic `cargo run-<board>` aliases invoke `probe-rs`
directly rather than `probe-rs-runner`, so they get neither the
`RUST_LOG=info` default nor the silent-exit hint described above.

## Verification via prometheus

Pool-side mining stats live in prometheus on the NUC, scraped from
ckpool's exposition file at `/var/lib/node_exporter/textfile/ckpool.prom`.
This is the source of truth for "is the board mining?" — board HTTP
pages can lag or show stale data after a reflash.

```sh
# Per-board hashrate from the pool's perspective, kH/s.
curl -sS 'http://redacted50.69/prometheus/api/v1/query?query=ckpool_client_hashrate_bps' \
  | jq -r '.data.result[] | "\(.metric.worker): \(.value[1] | tonumber / 1000) kH/s"'
```

Other useful series exported by `ckpool-exporter`:

| Metric | Meaning |
|--------|---------|
| `ckpool_client_hashrate_bps` | rolling 1-min hashrate (`dsps1 * 2^32`) |
| `ckpool_client_dsps{window=...}` | diff-shares-per-second, 1m / 5m / 1h |
| `ckpool_client_diff` | current pool difficulty for the worker |
| `ckpool_client_best_diff` | best share difficulty seen |
| `ckpool_client_last_share_seconds` | unix timestamp of the last share |
| `ckpool_client_idle` | 1 if idle, 0 otherwise |

The `worker` label matches the suffix in `DEFAULT_POOL_USER` (e.g. `rp2040`,
`esp32s3`). A worker that disappears from the result set has
disconnected from the pool entirely.

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
  via `eth_examples::cpu_load::run_cortex_m_executor` so the poll/wfe
  loop can bracket `wfe` with `Instant::now()` reads.
- The esp32s3 firmware uses `#[esp_hal::main]` and constructs the
  esp-rtos embassy executor manually so it can pass idle hooks to
  `Executor::run_with_callbacks`. The dummy-asic crate, which has no
  idle accounting, still uses the simpler `#[esp_rtos::main]`.
- All boards share network tasks, constants, buffers, and response
  building via the root `eth-examples` library crate.
- Embassy task constructors return `Result<SpawnToken, SpawnError>`;
  `spawner.spawn()` returns `()`. The `unwrap!` wraps the constructor.
- ESP32 crate pins `embassy-executor = "0.9.1"` to match `esp-rtos 0.2.0`
  internal dependency (workspace `0.10.0` is incompatible).
- UART for the dummy-asic link uses DMA on cortex-m targets:
  `embassy-stm32` `RingBufferedUartRx` for stm32f407/stm32f439, and
  `embassy-rp` `BufferedUart` (interrupt-driven, 1 KiB software ring
  buffer each direction) for rp2040/rp2350/bmm-pico. The RP hardware
  FIFO is 32 bytes; one-shot DMA would overflow it between `read()`
  calls at 921600 baud, and embassy-rp 0.10.0 ships no ring-buffered
  DMA RX. `BufferedUart` already implements `embedded_io_async::Read`
  and `Write` directly, so no newtype adapter is needed. The esp32s3
  host firmware and the dummy-asic peer stay
  on the interrupt+FIFO `Uart::into_async()` path because esp-hal
  1.0.0 ships no UART DMA driver. The closed-after-1.0.0 issues
  esp-rs/esp-hal#2642 (UART DMA support) and #4720 (circular UHCI RX)
  track this gap; revisit when the next esp-hal release reaches this
  workspace. The S3's 128-byte hardware FIFO plus its dual-core split
  absorb the ETH-ISR-class jitter that overran the 1-byte USART
  holding register on stm32f407.
