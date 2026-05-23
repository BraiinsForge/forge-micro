# API Refactor Size Budget

> Captured after the post-Phase-8 RAM regression fixes, at commit
> `430c3167`. Pre-refactor baselines were not captured at Phase 1;
> this is the first recorded data point on the surviving tree.
> Re-run the commands in "How to re-measure" against a future commit
> to diff.

## Build result

| Board     | Build result                                        |
|-----------|-----------------------------------------------------|
| bmm-pico  | OK                                                  |
| rp2040    | OK                                                  |
| rp2350    | OK                                                  |
| stm32f407 | OK (CCM RAM in use)                                 |
| stm32f439 | OK (CCM RAM in use)                                 |
| esp32s3   | OK                                                  |

## How the fix works

Three changes after the initial Phase 9 measurement uncovered
overflows:

1. `Metrics::MAX_METRICS` dropped from 128 to 64 and the
   `MetricKey` buffer from `heapless::String<128>` to
   `heapless::String<64>`. Recovered roughly 14 KiB on every
   board.
2. The `stm32f4: Place non-DMA static state in CCM RAM` change
   ships per-firmware `memory.x` files for `stm32f407` and
   `stm32f439` that declare the 64 KiB Core-Coupled Memory
   region embassy-stm32's default linker leaves unused. CCM has
   no DMA path and no instruction fetch, so only CPU-touched
   static state moves there.
3. `ubos-metrics` and `ubos-api` gain a `ccmram-bss` feature that
   annotates the metric map, HTTP request and response scratch
   buffers, the metrics scratch buffers, and the
   credential / session / CSRF / CSPRNG statics with
   `link_section = ".ccmram.bss"`. Only the two stm32f4
   firmwares enable the feature; on every other board the
   annotations are no-ops.

## Per-board release sizes

Section values from `cargo size --release -- -A`. RAM totals come
from the per-firmware `memory.x` (`rp2040`) or the embassy-stm32
auto-generated layout for boards that have not been overridden.

### RP and STM32 boards

| Board     | .text (KiB) | .rodata (KiB) | .data (KiB) | .bss (KiB) | .ccmram.bss (KiB) | Flash used | Flash total | Flash % | Main RAM used | Main RAM total | Main RAM % | CCM used | CCM total | CCM % |
|-----------|-------------|---------------|-------------|------------|-------------------|------------|-------------|---------|---------------|----------------|------------|----------|-----------|-------|
| bmm-pico  | 354         | 103           | <1          | 242        | —                 | 458 KiB    | 2048 KiB    | 22.3%   | 242 KiB       | 512 KiB        | 47.3%      | —        | —         | —     |
| rp2040    | 375         | 103           | <1          | 240        | —                 | 478 KiB    | 2048 KiB    | 23.4%   | 240 KiB       | 256 KiB        | 93.8%      | —        | —         | —     |
| rp2350    | 355         | 103           | <1          | 243        | —                 | 459 KiB    | 2048 KiB    | 22.4%   | 243 KiB       | 512 KiB        | 47.5%      | —        | —         | —     |
| stm32f407 | 355         | 104           | <1          | 126        | 24                | 459 KiB    | 1024 KiB    | 44.8%   | 126 KiB       | 128 KiB        | **98.4%**  | 24 KiB   | 64 KiB    | 38%   |
| stm32f439 | 356         | 104           | <1          | 174        | 24                | 460 KiB    | 2048 KiB    | 22.5%   | 174 KiB       | 192 KiB        | 90.7%      | 24 KiB   | 64 KiB    | 38%   |

Raw section bytes:

| Board     | .text   | .rodata | .data | .bss    | .ccmram.bss |
|-----------|---------|---------|-------|---------|-------------|
| bmm-pico  | 362,232 | 105,668 | 64    | 247,596 | —           |
| rp2040    | 384,324 | 105,652 | 64    | 246,164 | —           |
| rp2350    | 363,656 | 105,628 | 64    | 248,364 | —           |
| stm32f407 | 363,772 | 106,212 | 88    | 128,972 | 24,812      |
| stm32f439 | 363,920 | 106,204 | 88    | 178,124 | 24,812      |

### ESP32-S3

Section bytes from `xtensa-esp-elf-size -A`. esp-rtos splits the
binary into IROM, DROM, IRAM, and DRAM segments; `cargo size`'s
default `-A` output flattens them.

Earlier measurement (post Phase 8 + metric-cap trim, unchanged by
the stm32f4 CCM commit because the ESP32-S3 firmware does not
enable `ccmram-bss`):

| Section     | Bytes   | Notes                                      |
|-------------|---------|--------------------------------------------|
| .text       | 499,573 | IROM (executed via flash cache)            |
| .rodata     | 120,616 | DROM (read via flash cache)                |
| .rodata_desc|     256 | DROM segment descriptor                    |
| .rwtext     |   7,620 | IRAM code (copied from flash at boot)      |
| .vectors    |   1,024 | IRAM vector table                          |
| .data       |   7,704 | DRAM initialised data (copied from flash)  |
| .bss        | ~245,000| DRAM zero-initialised (post metric trim)   |
| .noinit     |   1,024 | DRAM uninitialised                         |

| Metric           | Used    | Total   | %     |
|------------------|---------|---------|-------|
| Flash (code+data)| 622 KiB | 8192 KiB|  7.6% |
| DRAM (static)    | ~249 KiB|  320 KiB| 77.8% |
| IRAM             |   8 KiB |  192 KiB|  4.4% |

## Headroom

- **bmm-pico**, **rp2350**: 47% main RAM used, > 50% headroom. No
  concerns.
- **rp2040**: 94% main RAM used (16 KiB headroom). Further static
  RAM growth needs new placement strategy — SRAM4 is not mapped by
  the embassy-rp default linker. Acceptable for now but flagged for
  follow-up if any new feature lands on this board.
- **stm32f407**: 98% main RAM used. Tight but within budget; the
  remaining 40 KiB of CCM is available if more static state needs
  to move. New CCM-eligible statics should be annotated with
  `#[cfg_attr(feature = "ccmram-bss", link_section = ".ccmram.bss")]`.
- **stm32f439**: 91% main RAM used; 40 KiB of CCM still free.
  Comfortable.
- **esp32s3**: 78% DRAM. Comfortable.

## How to re-measure

```sh
nix develop .#arm -c bash -c 'cd firmware/rp2040    && cargo build --release && cargo size --release -- -A'
nix develop .#arm -c bash -c 'cd firmware/rp2350    && cargo build --release && cargo size --release -- -A'
nix develop .#arm -c bash -c 'cd firmware/bmm-pico  && cargo build --release && cargo size --release -- -A'
nix develop .#arm -c bash -c 'cd firmware/stm32f407 && cargo build --release && cargo size --release -- -A'
nix develop .#arm -c bash -c 'cd firmware/stm32f439 && cargo build --release && cargo size --release -- -A'
nix develop .#esp -c bash -c 'cd firmware/esp32s3   && cargo build --release'
nix develop .#esp -c bash -c 'xtensa-esp-elf-size -A /home/jkr/projects/rusty-boards-api/build/target/xtensa-esp32s3-none-elf/release/esp32s3'
```
