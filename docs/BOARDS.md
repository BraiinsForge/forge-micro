# Development boards

Board facts for the evaluation and adapter boards: the board table,
wiring and pin assignments, size budget, build and flash commands, and
per-board OTA specifics. These boards carry no product-specific
support; they are what keeps the OS honest across three MCU parts.
The hashnode and forge-axe product boards and the rp2350-bootloader
they flash with are covered in [OVERVIEW.md](OVERVIEW.md),
[SETUP.md](SETUP.md), [OTA.md](OTA.md), and
[STRUCTURE.md](STRUCTURE.md).

## Board table

Split out of the table in [OVERVIEW.md](OVERVIEW.md), which keeps the
product-board rows.

| Crate | MCU | Ethernet | Target |
|-------|-----|----------|--------|
| `firmware/rp2040` | RP2040 | W6100 (SPI) | `thumbv6m-none-eabi` |
| `firmware/rp2350` | RP2350A | W6100 (SPI) | `thumbv8m.main-none-eabihf` |
| `firmware/bmm-pico` | RP2350A (BMM Adapter) | W5500 (SPI) | `thumbv8m.main-none-eabihf` |
| `firmware/esp32s3` | ESP32-S3 | W5100S (SPI) | `xtensa-esp32s3-none-elf` |

The esp32s3 board's on-die temperature sensor is supported by
`ubos/esp32s3-tsens/`, since esp-hal 1.0.0 does not expose it. That
board is the crate's only consumer.

## Debug probe wiring (Pico 2 debugprobe to EVB-Pico)

| Probe GPIO | EVB-Pico GPIO | Function |
|------------|---------------|----------|
| GP2 | SWCLK | SWD clock |
| GP3 | SWDIO | SWD data |
| GP4 | GP0 | UART TX (probe to target) |
| GP5 | GP1 | UART RX (target to probe) |
| GND | GND | Ground |

## SWD wiring (ST-Link V3 to Pico/EVB-Pico)

The ST-Link V3 uses a 14-pin STDC14 connector. The Pico debug header has
3 pins (SWCLK, GND, SWDIO). A 4th wire is required for voltage sensing --
without it the ST-Link reads 0V and refuses to connect.

| ST-Link V3 (STDC14) | Target |
|----------------------|--------|
| Pin 1 -- T_VCC | 3V3 (from Pico GPIO header) |
| Pin 2 -- SWDIO | SWDIO |
| Pin 3 -- GND | GND |
| Pin 4 -- SWCLK | SWCLK |

T_VCC does not power the target. It is a voltage-sense input that sets the
SWD I/O levels. The target must be powered separately via USB.

## Pin assignments

### W6100-EVB-Pico (rp2040)

| Function | GPIO | SPI0 |
|----------|------|------|
| MISO | 16 | SPI0 RX |
| CS | 17 | GPIO |
| CLK | 18 | SPI0 SCK |
| MOSI | 19 | SPI0 TX |
| RST | 20 | GPIO |
| INT | 21 | GPIO (Pull-Up) |
| LED | 25 | GPIO |

This board also runs on the BMM Adapter carrier. For the fan wiring on
that carrier, see [Fan connector](#fan-connector).

### W6100-EVB-Pico2 (rp2350)

Same pinout and the same W6100 as the W6100-EVB-Pico, on an RP2350.
This board also runs on the BMM Adapter carrier. For the fan wiring on
that carrier, see [Fan connector](#fan-connector).

### BMM Adapter (bmm-pico)

RP2350A on the BMM Adapter carrier board with a W5500 over SPI0 and the
chip-side resources required by the ubos-mining backend on UART1, I2C1, and GPIO.
See `firmware/bmm-pico/src/config.rs` for the full peripheral map. The
ubos-mining bring-up is in [MINING.md](MINING.md).

The carrier accepts any of the three RP devkit modules, so `rp2040` and
`rp2350` run on it as well and inherit its connector wiring. The note
below therefore applies to all three boards, not to `bmm-pico` alone.

#### Fan connector

The carrier's schematic names J13 pin 3 `FAN_PWM` and pin 4 `FAN_RPM`,
but the Intel 4-wire fan spec assigns PWM to pin 4 and TACH to pin 3.
The firmware follows the spec, so the cable between the connector and
the fan must swap pins 3 and 4 relative to the schematic net names.

TODO: Drop this note when a future board revision renames the nets.

### ESP32-S3 + W5100S (esp32s3)

Repurposes the LCD header pins on an ESP32-S3 dev board.

| Function | GPIO |
|----------|------|
| SCLK | 1 |
| MOSI | 2 |
| MISO | 42 |
| CS | 39 |
| INT | 41 (Pull-Up) |
| RST | 40 |

## PCB sources

The BMM Adapter's KiCad sources, and the ERC/DRC and fabrication
tooling gating them, live in the internal repository. The adapter's
pin assignments above are what the firmware needs.

## Building and flashing

ARM targets share one dev shell:

```sh
nix develop .#arm
cd firmware/rp2040    && cargo run --release
cd firmware/rp2350    && cargo run --release
cd firmware/hashnode  && cargo run --release
cd firmware/bmm-pico  && cargo run --release
```

ESP32-S3 targets need the ESP dev shell:

```sh
nix develop .#esp
cd firmware/esp32s3 && cargo run --release
```

The esp32s3 boots through the ESP-IDF second-stage bootloader and an OTA
partition table. A board that has never been provisioned needs the
rollback-enabled bootloader and the partition table flashed alongside the app;
`probe-rs run` takes them via `--idf-bootloader`, `--idf-partition-table`, and
`--idf-target-app-partition`. See [ESP32-S3 OTA](#esp32-s3-ota) below
for the full provisioning command and the `nix run .#esp32s3-image`
OTA image build.

## Size budget

Rows for the boards in this repository, split out of the footprint
table in [STRUCTURE.md](STRUCTURE.md); the snapshot caveats and the
measurement recipe are there. A row marked "est." is not measured. It
is the last measurement plus the larger metrics render buffer. Every
row was measured with the fee switch off. On a board with four
sessions, a build that ships the fee slot adds one session task
future to Main RAM. `rp2040` and `esp32s3` run three sessions in both
states, so their rows hold in both states.

| Board     | Flash     | Main RAM   | Headroom note                                  |
|-----------|-----------|------------|------------------------------------------------|
| bmm-pico  | 566 / 2048 KiB | ~406 / 512 KiB (79%, est.) | comfortable; ~106 KiB free     |
| rp2040    | 589 / 2048 KiB | 214 / 256 KiB (84%) | ~42 KiB free for the stack, which reaches ~20 KiB at boot. Ships without the metrics store, because with it only ~15.4 KiB stays free and the stack overflows |
| rp2350    | 567 / 2048 KiB | ~406 / 512 KiB (79%, est.) | comfortable; ~106 KiB free     |
| esp32s3   | 854 / 8192 KiB | 263 / 320 KiB DRAM (82%) | ~61 KiB DRAM free for the stack, which must stay above 24 KiB. Ships without the metrics store, because with it the free stack falls below that floor. Holds 64 series when built with the `metrics` feature |

The metrics store costs the RAM below. Each number is the difference
between two release builds of one tree, with the `metrics` feature on
and off, measured on 2026-11-24 for the esp32s3 and on 2026-11-26 for
the rp2040. RAM is the sum of `.data`, `.bss` and
`.uninit`, or `.noinit` on the esp32s3. Flash is the sum of `.text` and
`.rodata`. The number holds the map, the render buffer, the channel,
the listener sockets and the task futures of the listener pool.
[METRICS.md](METRICS.md) describes the feature.

| Board   | Series | RAM with the store | RAM without | Store RAM | Store flash |
|---------|--------|--------------------|-------------|-----------|-------------|
| esp32s3 | 64     | 292.2 KiB          | 256.9 KiB   | 35.3 KiB  | 13.2 KiB    |
| rp2040  | 64     | 240.6 KiB          | 213.9 KiB   | 26.7 KiB  | 9.1 KiB     |

Reproduce the column without the store with the build command of
[METRICS.md](METRICS.md) and the size command of that board.

Reproduce the esp32s3 numbers with the commands below. The workspace
puts its build output at the root, so the size tool is pointed there and
not at the crate directory. DRAM sums `.data`, `.bss` and `.noinit`, the
sections that match the `.data`/`.bss`/`.uninit` rule the other rows
use.

```sh
nix develop .#esp -c bash -c 'cd firmware/esp32s3 && cargo build --release'
nix develop .#esp -c bash -c 'xtensa-esp-elf-size -A \
  target/xtensa-esp32s3-none-elf/release/esp32s3'
```

## OTA

The board-agnostic upload flow, the embassy-boot swap mechanism, and
the diagnostics are in [OTA.md](OTA.md). The rp2350, rp2040, bmm-pico,
and esp32s3 boards support OTA; the per-board material below is split
out of that document.

### Validation status

- The full upload → swap → confirm → rollback cycle was validated on
  the rp2350 on 2026-06-18: v1 → v2 → deliberately broken v3 (hung
  before `mark_booted`) → auto-revert to v2 → v4 passed end to end.
- rp2040 and bmm-pico are at flash-write parity with rp2350 — the
  core-1 park across `begin`/`write_chunk`/`finish`/`reboot_into_new`
  and an `abort()` that releases the park and resets `write_offset` —
  and share the same custom `#[panic_handler]`,
  `record_panic_breadcrumb`, and `publish_reset_diag` path. What
  remains is hardware validation: only the rp2350 has been exercised
  through a full cycle, and the other two boards need the same before
  they can be trusted for OTA.
- esp32s3 is code-complete and gated in CI, but the upload → activate
  → rollback cycle has not yet been run on hardware. Two interactions
  need on-board confirmation: the engine's `ubos_mining` core-1 park
  composed with `esp-storage`'s `multicore_auto_park` during a flash
  write, and whether the nix-built rollback bootloader reverts a
  broken image as expected.

### ESP32-S3 OTA

The esp32s3 reaches the same OTA behaviour through a different mechanism. There
is no Rust second-stage bootloader for Xtensa (embassy-boot targets ARM and
RISC-V only), so the swap and rollback live in the ESP-IDF C bootloader, and the
firmware drives it through the `otadata` partition via `esp-bootloader-esp-idf`.
The shared upload flow, WebUI, CSRF, session lifecycle, and `/api/info`
diagnostics in [OTA.md](OTA.md) apply unchanged; only the flash layout,
bootloader, and per-board specifics below differ.

#### Flash layout

The 8 MiB flash uses an ESP-IDF partition table (`firmware/esp32s3/partitions.csv`,
flashed at `0x8000`):

| Name      | Type | SubType | Offset     | Size   | Purpose |
|-----------|------|---------|------------|--------|---------|
| `otadata` | data | ota     | `0x9000`   | 8 KiB  | Selects the active app slot; two redundant copies |
| `ota_0`   | app  | ota_0   | `0x10000`  | 3 MiB  | App slot A |
| `ota_1`   | app  | ota_1   | `0x310000` | 3 MiB  | App slot B |
| `config`  | data | `0x40`  | `0x610000` | 1.9 MiB| ubos-config store (uses the first 8 KiB) |

There is no `factory` slot: an empty `otadata` boots `ota_0`. There is no `nvs`
or `phy_init` partition because the board has no esp-wifi. The running image is
~760 KiB, well under the 952 KiB OTA upload limit and a fraction of the 3 MiB
slot. `config::Flash::CONFIG_RANGE` (`0x610000..0x612000`) must stay in sync with
the `config` partition offset.

Provisioning this table rewrites the flash layout, so a board carrying config
from an earlier layout loses it and boots from defaults. This only happens
during the one-time probe-rs provision below, not across an OTA update.

#### Bootloader

The ESP-IDF second-stage bootloader is built by the `esp32s3-bootloader` flake
package from the minimal stub project in `firmware/esp32s3-bootloader/`. Its only
non-default setting is `CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE=y`: the bootloader
that espflash and probe-rs bundle has rollback **disabled**, so the
`PendingVerify` auto-revert would never fire. The build uses ESP-IDF from the
`nixpkgs-esp-dev` flake input and is gated in CI by `build-esp32s3-bootloader`.

#### Backend and boot

`EspBackend` (`ubos/ubos-ota/src/esp.rs`, feature `esp`) implements `OtaBackend`
over `esp-bootloader-esp-idf`'s `OtaUpdater`:

1. `write` streams chunks into the inactive slot. `esp-storage`'s `Storage::write`
   does read-modify-erase-write per 4 KiB sector, so no per-sector updater dance
   is needed (unlike the RP backend).
2. `activate` flips `otadata` to the freshly written slot and marks it
   `OtaImageState::New`. The rollback-enabled bootloader boots a `New` slot once
   as `PendingVerify` and reverts on the next reset unless it is confirmed.
3. `reboot` calls `esp_hal::system::software_reset()`.

At boot, `ubos_ota::confirm_running` marks a `PendingVerify` slot `Valid`,
cancelling the pending revert — the analogue of the RP `mark_booted`. A crash or
hang before that call leaves the slot unconfirmed, so the next reset reverts to
the previous slot.

The single `esp-storage` flash driver is shared between the config store and the
OTA writer through `ubos_ota::EspFlashMutex`; `multicore_auto_park` halts core 1
around each flash op so a write never races the mining serial worker. The
core-1 park that the engine performs before an upload still applies.

#### Build and flashing

Build the OTA image (the esp-idf app image the upload endpoint expects):

```sh
UBOS_FW_VERSION=42 nix run --impure .#esp32s3-image   # -> build/esp32s3-v42-<build id>.bin
```

`espflash save-image` produces the app-image format; the `esp32s3-image`
package rejects an image over the 952 KiB upload limit.

A fresh board needs the rollback bootloader and partition table flashed
alongside the app; the plain `cargo run` runner does **not** pass the esp-idf
flags, so it would flash the app on top of whatever bootloader is already
present (espflash's default has rollback **disabled**). Provision in one shot
with the idf flags — the `esp32s3-bootloader` package supplies the rollback
bootloader and the partition table is `partitions.csv`:

```sh
bl="$(nix build --print-out-paths .#esp32s3-bootloader)/bootloader.bin"
nix develop .#esp -c bash -c "cd firmware/esp32s3 && \
  cargo build --release && \
  probe-rs run --chip esp32s3 \
    --idf-bootloader \"$bl\" \
    --idf-partition-table partitions.csv \
    --idf-target-app-partition ota_0 \
    target/xtensa-esp32s3-none-elf/release/esp32s3"
```

Once provisioned, the bootloader and partition table persist, so the day-to-day
dev loop reflashes only the app:

```sh
nix develop .#esp -c bash -c "cd firmware/esp32s3 && cargo run --release"
```

Day-to-day OTA needs no probe at all: upload `esp32s3-vN-<build id>.bin` through the WebUI
or `POST /api/system/OTA`.

#### ESP-specific diagnostics

The esp32s3 has no reset-surviving scratch registers like the RP watchdog block,
so the OTA breadcrumb is not recorded: `resetOtaActive` is always `false`,
`resetOtaOffset` is `0`, and `resetWritePhase` is `"none"`. `resetReason` is
still populated from the chip's reset reason. `hasPreviousImage` is `false` and
`previousVersion` is `null` — rollback is bootloader-driven, not app-reported.
The two-slot `otadata` scheme is self-healing: a reset mid-upload simply leaves
`otadata` pointing at the old slot, so no breadcrumb is needed to recover.
