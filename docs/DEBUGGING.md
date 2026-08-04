# Debugging and analysis

## probe-rs

- `probe-rs list` shows the connected probes.
- `cargo run` flashes over a probe attached to this machine. When only
  one probe is attached, the runner selects it automatically.
- When several probes are attached, export `$PROBE_SERIAL` to select
  one. The runners give that serial to `probe-rs` as `--probe`.

### A wedged probe

If `cargo run` returns silently -- no defmt output, while the board's
uptime keeps growing -- the CMSIS-DAP probe is wedged per
probe-rs/probe-rs#1849. `probe-rs-runner` defaults `RUST_LOG=info`, which
surfaces "Could not determine a suitable packet size for this probe"
instead of exiting silently.

Recovery is a physical unplug and replug of the affected debugprobe.
`uhubctl` and software USB resets do not help through a hub that reports
`ppps` without actually cutting power. `probe-rs info` is what triggers
the wedge, so never run it twice in a row on the same probe.

## Binary size

`cargo size --release -- -A` (from `cargo-binutils`, available in the dev
shells) prints per-section sizes from the ELF. It builds first, so it is
a drop-in replacement for `cargo build --release` when you want size
output.

Useful budgets:

- Flash = `.vector_table + .text + .rodata + .data` (plus `.boot2` on
  RP2040; plus `.start_block + .bi_entries + .end_block + .gnu.sgstubs`
  on RP2350)
- Static RAM = `.data + .bss + .uninit`

`cargo bloat --release` attributes flash usage to functions and crates.

## Stack

The executor model determines what "stack per task" means:

- ARM (`embassy-executor`): a single shared stack used by whichever task
  is polling. Task state lives in a static arena sized at compile time.
  Worst-case stack usage is the deepest poll path plus ISR nesting.
- ESP32-S3 (`esp-rtos`): FreeRTOS threads with per-thread stacks sized at
  spawn. The embassy executor runs on one of them.

Analysis options:

- `cargo-call-stack` (Cortex-M): whole-program static upper bounds per
  entry point, with known limitations around async state machines and
  function pointers.
- `-Z emit-stack-sizes` (nightly rustc): per-function sizes emitted to a
  linker section; no call graph, so pair with external analysis.
- Runtime watermark: paint the stack with a known pattern at boot and
  read the high-water via probe-rs. On ESP32-S3, FreeRTOS exposes
  `uxTaskGetStackHighWaterMark` directly.

## Known investigations

- [rp2350-wake-storm.md](rp2350-wake-storm.md) -- the RP2350 WFE
  wake-storm that drove the `InterruptExecutor` switch in the rp2350
  firmware.
