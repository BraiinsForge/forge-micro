# Hashboard EEPROM Support

Every hashboard (`BMM101_HASHBOARD` on the miniminers, `BFMX4_HASHBOARD`
on the Hashnode) carries an EEPROM with factory-programmed operational
data: product name, serial number, batch, ASIC type/count, and the
default voltage/frequency/hashrate/power operating point. `ubos-eeprom`
reads and parses this data once at boot, reusing `bos-main`'s existing
read-only wire format so hashboards already flashed by its factory
tooling read correctly here without a second format or flasher.

## Naming corrections

Descriptor and control-board names had drifted from the real hardware.
Fixing this first kept the EEPROM work from naming the wrong board:

- **`ubos_asic::BMM100` → `BMM101_HASHBOARD`.** The hashboard this
  firmware targets carries `BMM101`'s sensor layout (LM75A at
  `0x49..=0x4c`; `BMM101_SENSORS` already reflected this correctly).
  `BMM100` was an earlier hardware revision (sensor at `0x48` instead)
  that this firmware never targeted — the const name was simply wrong,
  not a second supported board. Renamed pervasively: the const
  definition, every call site, and prose comments that describe "the
  board this firmware runs on" now read `BMM101`. One exception:
  `ubos-psu/src/config.rs`'s `BmmPsuDescriptor::DEFAULT` doc comment
  cites `bos-main`'s actual `BMM100_PSU_DESCRIPTOR` symbol by name
  (that PSU config is shared verbatim between the BMM100 and BMM101
  boards in `bos-main`, so the symbol really is called that) — that
  citation stays as `BMM100` for traceability, with a note that it's
  shared across both revisions.
- **`ubos_asic::HASHNODE` (hashboard descriptor) → `BFMX4_HASHBOARD`**;
  **`ubos_machine::HASHNODE_SENSORS` → `BFMX4_SENSORS`.** "Hashnode" is
  the product codename for the whole integrated PCB (control board +
  hashboard + PSU); "BFMX4" (Braiins Forge Miner x4) is the hashboard
  model, the same relationship as miniminer/BMM101. The machine-level
  `ubos_machine::HASHNODE` const and `CONTROL_BOARD_HASHNODE` stay —
  they name the product, not the hashboard.
- **Control-board display names**, corrected against the real
  hardware:
  - `CONTROL_BOARD_RP2350.name`: `"Pico2 W6100"` → `"W6100-EVB-Pico2"`.
  - `CONTROL_BOARD_BMM_PICO` → `CONTROL_BOARD_BMM_ADAPTER` (and
    `MINIMINER_BMM_PICO` → `MINIMINER_BMM_ADAPTER`); display name
    `"BMM Adapter W5500"` → `"W5500-EVB-Pico2"`. There is no
    "bmm-pico" product — the real hardware is a genuine Wiznet
    W5500-EVB-Pico2 module mounted on Braiins' own "BMM Adapter"
    carrier PCB (`pcb/bmm-adapter/`, whose schematic literally labels
    the mounted module `W5500-EVB-Pico2`). This rename is scoped to
    the `ubos-machine` Rust identifiers and display string only; the
    `firmware/bmm-pico` crate directory/package name and its ~100
    references across `flake.nix`/`CLAUDE.md`/docs (47 in `flake.nix`
    alone) are left as-is — a larger, separate blast radius (release
    artifact names, CI check names).
  - `CONTROL_BOARD_ESP32S3.name`: `"ESP32-S3 W5100S"` →
    `"Waveshare ESP32-S3-LCD-Driver-Board W5100S"`, matching the real
    hardware named in `firmware/esp32s3/src/main.rs`'s SPI-wiring
    comment.
  - `CONTROL_BOARD_RP2040` (`"W5500-EVB-Pico"`), STM32F407
    (`"DevEBox STM32F407 LAN8720"`), and STM32F439
    (`"Nucleo-F439ZI LAN8742A"`) were already accurate — no change.

## EEPROM wire format

Matches `bos-main`'s braiinsminer hashboard EEPROM layout byte-for-byte,
so boards already factory-programmed by the existing `bmm-flasher` tool
read correctly and no second format/flasher exists:

- 256 bytes total, two fixed-offset sections.
- `BoardInfoSection` (192 bytes): magic `b"Br"`, a version byte
  (`Version0`), product name/version/batch/serial (16-byte
  NUL-terminated ASCII fields), ASIC type/bin, chip/domain/sensor
  counts, default voltage/frequency/hashrate/power/temperature,
  `0xFF`-padded reserved bytes, trailing CRC-16/CCITT
  (`CRC_16_IBM_3740`) over the section.
- `FirmwareInfoSection` (64 bytes): magic `b"Br"`, MCU firmware
  version/compile date/device id/serial, reserved bytes, its own CRC.
- Physical part in `bos-main`: M24C02 (256 B, 16-byte pages) at base
  I2C address `0x50`.

Parsing is a fallible conversion (`TryFrom<[u8; 256]>`-shaped): length
is enforced at compile time by the `&[u8; EEPROM_LEN]` parameter type;
the parse validates both magics, the version enum, both CRCs, and the
reserved-byte padding, in that order, matching `bos-main`'s validation
order exactly so a real captured dump either parses or fails the same
way in both codebases.

A text field that fails to decode (invalid UTF-8, or non-ASCII) does
not fail the whole record: it falls back to the literal string
`"INVALID"`, matching `bos-main`'s own `StringText` helper. A real
BMM101 hashboard dump surfaced a CRC-verified record with one text
field padded past its content with `0xFF` and no NUL terminator; an
earlier, stricter pass here failed the whole record over that one
field, which didn't match `bos-main`'s behavior on the same dump.

## Bus-agnostic trait

Other EEPROM chips (and buses other than I2C — 1-Wire in particular)
are expected to show up on future hardware, so the format-parsing layer
does not depend on I2C at all. This follows the same shape as
`ubos_psu::Psu`: a plain trait with no bus type in its signature.

```rust
pub trait HashboardEeprom {
    type Error;
    async fn read(&mut self, buf: &mut [u8]) -> Result<(), Self::Error>;
    async fn log_identity(&mut self) where Self::Error: core::fmt::Debug { ... }
}
```

`log_identity` is a provided method: the whole read/parse/log boot path
lives once on the trait, so each board contributes only its bus handle
instead of carrying a per-firmware copy. It logs through the
`ubos-logging` facade, because the redacted builds its `ubos-*` crates on
the `log` backend while every other board uses `defmt`. The numeric
fields are logged alongside the identity strings deliberately: the CRC
validates bytes, not semantics, so a byte-order mistake would parse
cleanly and only a human reading the values catches it.

`ubos-eeprom` is `no_std`, sitting alongside `ubos-psu` /
`ubos-temperature` in the [machine model](MACHINE.md):

- `format` module: the wire-format types and the fallible parse.
  Depends on nothing but `crc`; text fields borrow `&str` directly out
  of the caller's buffer instead of copying into fixed-capacity
  storage. Unit-tested on host against synthetic byte dumps — no
  hardware needed.
- `i2c` module: `I2cEeprom<I: embedded_hal_async::i2c::I2c>`, the only
  concrete `HashboardEeprom` impl today (address passed at
  construction like `ubos_temperature::lm75a::Lm75a`; reads are not
  page-limited, the whole chip comes back in one transaction). A
  future 1-Wire chip implements the same trait in its own module
  without touching `format` or any call site typed against
  `HashboardEeprom`.
- Gated in `flake.nix` checks (build + clippy + host test) alongside
  `ubos-psu`/`ubos-temperature`, per the verification requirement that
  every crate is gated.

## Boot-time wiring

- `HashboardDescriptor` carries an `eeprom_i2c_address: Option<u8>`
  field (wire-level data, same tier as the PSU/sensor addresses already
  on descriptors): `Some(0x50)` for `BMM101_HASHBOARD`, `Some(0x57)`
  for `BFMX4_HASHBOARD`.
- Every board firmware (bmm-pico, hashnode, rp2040, rp2350, stm32f407,
  stm32f439, esp32s3, redacted) reads the EEPROM once at boot, before
  powering the rail, and logs the parsed product name/serial/asic/batch
  plus the default operating point. On all boards but the redacted the
  EEPROM shares the PSU/sensor I2C bus, so the read borrows a fresh
  `I2cDevice` off the existing bus mutex; the redacted borrows the STM8
  fan-tach bus directly before the fan task takes it, so no bus-sharing
  plumbing needed to be added there.
- A read or parse failure only logs a warning. This is a sanity check
  and traceability source, not a config source: descriptors stay
  compile-time (per [MACHINE.md](MACHINE.md)'s no-runtime-autodetection
  stance) regardless of what the EEPROM reports.
- Read-only. Writing stays out of firmware scope, matching `bos-main`
  (factory writes go through a separate flasher tool, not the mining
  daemon).

## Confirmed on hardware

`0x50` is correct: on a real bmm-pico + BMM101 hashboard, the boot log
reads `hashboard EEPROM: product=BMM101 P00 serial=SN asic=BM1362AC
batch=BATCH` — `"BMM101 P00"` is the exact product-name string
`bos-main` registers for `BRAIINS_MINI_BMM101_BM1362_4X1`, confirming
the address, the wire format, and the `BMM101_HASHBOARD` rename all at
once.

## Known limitations

- `BFMX4_HASHBOARD`'s `0x57` address is wired into `firmware/hashnode`'s
  boot sequence but is tentative: unlike `BMM101_HASHBOARD`'s `0x50`,
  it has not been cross-checked against a parsed product-name match on
  real hardware. A wrong address only produces a bus error or a failed
  parse (both log-only), no effect on bring-up.
- Whether the hashboard EEPROM is physically reachable on the redacted's
  I2C1 (the redacted STM8 fan-tach bus) is not yet verified on hardware.
  The failure mode is the same log-only warning, so the boot log
  answers the question the first time a board runs this firmware.
- Serial number/batch are not surfaced through the `system_info` REST
  API: the boot-time read happens once, while the API handler runs
  later, possibly concurrently, which needs a runtime-shared cell —
  not implemented here.
- No mismatch warning exists between the EEPROM's `product_name` and
  the compiled descriptor: `HashboardDescriptor::model` holds a
  board-family name (`"miniminer"`, `"Braiins Forge Miner x4"`), while
  `product_name` holds a chip-board string (`"BMM101 P00"`) — the two
  are not in comparable formats, so a real check needs a per-hashboard
  product-name field first, not just a log line.
- The real EEPROM chip's part number on these specific boards is
  unconfirmed (affects page size for writes only; format and
  single-shot reads are unaffected either way).
