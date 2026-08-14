# Machine Descriptor Model

The miner is modeled as composed `const` data: every physical
component — asic chip, hashboard, PSU, temperature-sensor group,
control board, platform/MCU, machine — is described by a data
descriptor, and a `MachineDescriptor` composes them into the single
source of truth a firmware binary builds from. The firmware binary
supplies only the typed peripheral bindings that data cannot express.

## Descriptor hierarchy — each descriptor lives with its component

There is no central "descriptors" crate. Each descriptor is owned by
the crate that owns the component:

| Descriptor | Crate | Consts |
|------------|-------|--------|
| `ChipDescriptor` | `ubos-asic` | embedded in the `BmDescriptor` statics (`BM1362`, `BM1366`, `BM1370`) in the crate-internal `ubos_asic::bm13xx::chip` |
| `HashboardDescriptor` | `ubos-asic` | `BMM101_HASHBOARD`, `BFMX4_HASHBOARD` |
| `BmmPsuDescriptor` | `ubos-psu` | `BmmPsuDescriptor::DEFAULT` |
| `BfmPsuDescriptor` | `ubos-psu` | `BfmPsuDescriptor::DEFAULT` |
| `SensorDescriptor` | `ubos-temperature` | struct only; the tables (`BMM101_SENSORS`, `BFMX4_SENSORS`) live in `ubos-machine` |
| `PlatformDescriptor` | `ubos-machine` | `RP2040` .. `ESP32S3` |
| `ControlBoardDescriptor` | `ubos-machine` | `CONTROL_BOARD_*` |
| `MachineDescriptor<Psu>` | `ubos-machine` | `MINIMINER_*`, `HASHNODE` |

Two placements are deliberate:

- **The hashboard descriptor stays in `ubos-asic`.**
  `ChipProtocol::initialize_domains` and `max_rail_voltage` consume it
  inside asic bring-up; moving it to a hashboard crate that depends on
  `ubos-asic` would create a cycle. It is legitimately chip-chain
  topology.
- **`ubos-machine` is its own crate.** The composed descriptor
  references `ubos-asic`/`ubos-psu`/`ubos-temperature` types, so it
  sits above them, and no existing library crate occupies that layer
  with machine semantics. It carries no embassy or hardware
  dependencies, so it builds identically for ARM, Xtensa, and the
  host, and both composition roots — the firmware binaries and
  `host-runner` — read the same definitions.

Two components have no descriptor on purpose: the fan, because the
`ubos_cooling::FanOutput`/tach traits already abstract everything the
binary would otherwise configure, and mining identity (`MiningInfo`),
which `ubos-config` derives per board from the hashboard descriptor so
relocating it would buy nothing but a dependency edge.

`MachineDescriptor` is generic over its PSU type (`Psu: Copy`) rather
than wrapping a `Bmm`/`Bfm` sum type: no code ever handles a
`MachineDescriptor` generically, every board names its own concrete
`MINIMINER_*`/`HASHNODE` const, so a sum type would only add a
match arm each board must unwrap without ever exercising the other
side. The generic keeps the wrong PSU family for a board a type
error, not a possibility a `match` has to rule out.

## The data / binding boundary

Embassy peripherals are singleton types (`Peri<'static, PIN_6>`) and
tasks are monomorphized, so pin and peripheral wiring is inherently
compile-time and type-level. What a descriptor field can be splits
three ways:

- **Tier 1 — genuinely data (the descriptor):** i2c addresses (they
  go on the wire, never into a type), voltage/frequency bounds,
  chip/domain counts, MAC family, HTTP pool sizes, event-log
  capacity, names.
- **Tier 2 — data-shaped but the mechanism stays code:** GPIO can be
  type-erased (`p.PIN_6.degrade()`), so a descriptor could hold
  "reset = pin index 3", but the index only resolves through a
  board-authored table that names each singleton. Bus instances
  (`I2C0`/`I2C1`, PWM slices, DMA channels) are distinct types with
  no erased constructor and must be named in code.
- **Tier 3 — cannot be data under any scheme:** which HAL, which MCU,
  and driver types — e.g. the W6100 and W5500 Ethernet PHYs are
  different Rust types. Selecting between them by data means
  compiling both drivers into every image behind an `enum`/`dyn`
  plus a per-board table: more code and runtime dispatch for nothing.

## One binary crate per control board

Binaries stay one-per-control-board, not one-per-MCU, even where
boards share an MCU (rp2350 and bmm-pico are both RP2350A; hashnode's
RP2350B is the same Cortex-M33 core and target and reuses the same
`PlatformDescriptor` const). Same-MCU boards differ in Tier 2 (pins)
and Tier 3 (PHY type), so their difference lives in the binding/type
layer, not in data; two thin binaries are less code than one binary
carrying both PHY drivers.

Each binary crate is a thin composition root — the irreducible "how"
that names types and singletons, and nothing else:

```rust
// 1. init the HAL, extract peripherals                  (per MCU)
// 2. impl the hardware bindings: pins, buses, PHY, reset (per board)
// 3. name the machine it builds:
const MACHINE: MachineDescriptor<BmmPsuDescriptor> = ubos_machine::MINIMINER_RP2350;
```

The descriptor declares *what* the machine is; the binding supplies
the typed handles. Everything expressible as data is data; the
binding is the small honest residue.

The task shells are part of that residue, and most of them are not. A
board takes each shell from a macro. That macro lives in the crate that
owns the loop the shell calls, beside the loop itself:
`ubos_core::watchdog_task!` sits next to
`ubos_core::watchdog::run_watchdog`. Three shells belong to no such
crate and stay in `ubos/ubos-firmware/src/tasks.rs`.

Four names are the exception, and a board can still declare them by hand:

`boot`, `cpu_load_task`, `mdns_task`, `stratum_client_task`.

`boot` is the composition root itself. `mdns_task` and `cpu_load_task`
name per-board consts, and `stratum_client_task` needs the board's own
session type and pool size.

The `board-skeleton-parity` check holds boards to that list. It also
fails a board that does not name `ubos_firmware::net::bring_up`,
`ubos_firmware::events::install` and `ubos_firmware::serve_all!`. A board
that writes its own boot sequence writes plain functions, which carry no
task attribute, so the name list alone cannot see it. A board with tasks
of its own names them at the call site in `nix/checks.nix`. Keep this
list and that call site level with each other.

## Layering

```
ubos-core
 ├ ubos-asic (chip + hashboard)  ├ ubos-psu  ├ ubos-temperature
 │    └ ubos-config (derives MiningInfo/bounds from the hashboard)
 └──────┴───────────┴────────────┐
                            ubos-machine
                                 └ firmware/* + host-runner
```

`host-runner` is not one of the boards and has no
`MachineDescriptor`; it reads the base descriptors directly —
`BmmPsuDescriptor::DEFAULT` and `ubos_machine::BMM101_SENSORS`, the same
consts the machines wrap — so it cannot drift from the firmware.

## Follow-up work (not in this model yet)

- **Runtime auto-detection** of attached hashboards / PSU / control
  board, and a **generic machine builder** that spawns tasks from a
  `MachineDescriptor` plus a `HardwareBindings` trait — the
  formalized binding seam. The per-platform support crates now hold the
  shareable per-MCU work: `ubos-rp`, `ubos-stm32`, `ubos-esp32` and
  `ubos-build`. The builder no longer has to hoist it.
- **Multiple hashboards per machine.** `MachineDescriptor.hashboard`
  is a single field, expanded to a collection when multi-board wiring
  actually lands.
- **New control boards.** The `PlatformDescriptor` shape
  anticipates them.
