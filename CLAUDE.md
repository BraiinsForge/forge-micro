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
