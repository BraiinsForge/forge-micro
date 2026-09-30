# Repository Structure

Status: implemented — the code wins. When this file and the code
disagree, the code is right and this file needs a patch.

Target layout for the repository tree. This document is the
authoritative source for where new code, libraries, firmware crates,
patches, and assets belong. It exists because the previous layout
mixed workspace root, library crates, firmware binaries, host tools,
patches, and non-Rust assets at the top level with no naming
discipline; the rationale below records why the new shape was picked
and which authoritative references it is based on.

## Layout

```
.
├── Cargo.toml              virtual workspace manifest, no [package]
├── Cargo.lock
├── .cargo/config.toml      workspace-wide cargo config and env
├── flake.nix flake.lock    flake entry point: inputs, wiring, outputs
├── nix/                    flake modules: toolchains, source, deps,
│                           crate graph, checks, images, devshells,
│                           guards, probe-rs, lib helpers
├── README.md               entry point, quick-start commands
├── CLAUDE.md               agent instructions, build conventions
├── docs/                   architecture, plans, this file
├── ubos/                   no_std library crates (firmware framework + drivers)
├── host/                   std library crates and host-side binaries
├── firmware/               per-target firmware binaries
├── patches/                upstream patch files applied at build time
└── micro-sv2/              nested workspace: vendored SV2 library
```

There is no `src/` at the workspace root. The root `Cargo.toml` is a
virtual manifest containing only `[workspace]`, `[workspace.dependencies]`,
`[workspace.lints]`, `[profile.*]`, and `[patch.crates-io]`.

## `ubos/` — no_std library crates

Every crate that compiles as `no_std` for embedded targets lives
here. Each is a sibling directory; the folder name matches the crate
name exactly. Naming uses prefixes to encode subsystem; `ls ubos/`
gives the framework's table of contents.

| Prefix      | Meaning                                    |
|-------------|--------------------------------------------|
| `ubos-`     | core firmware framework                    |
| `ubos-mining`, `mining-protocol`, `stratum-*` | mining domain    |
| `rp235x-`, `serial-`      | hardware-specific drivers |

The firmware framework is split along domain lines. `ubos-core`,
`ubos-logging`, and `ubos-config` are leaf crates with no
`ubos-*` dependencies above the logging facade; the chain
`(core + logging + config) ← ubos-net ← ubos-metrics ← ubos-display`
runs strictly one-way above them. `ubos-manager` sits beside the chain
and supervises the thermal/cooling/mining handles via watch channels;
hardware and protocol crates sit beside the chain without participating
in it. `ubos-machine`'s pure descriptor data sits below the mining and
protocol crates that read it: `ubos-mining`, `stratum-client`, and
`ubos-api` each depend on `ubos-machine` for `MAX_CHAINS` and the
per-board descriptor. `ubos-metrics` owns the per-chain accounting table
and its `MAX_CHAINS` ceiling, so `ubos-mining` writes it and `ubos-api`,
`ubos-thermal` and `ubos-display` read it, the last two without depending
on the mining crate.

`ubos-auth` depends on `ubos-logging` only and holds the shared HTTP
middleware (allow-list, auth, sessions, CSRF, CSPRNG). Both handler
crates `ubos-webui` and `ubos-api` depend on `ubos-auth`, `ubos-http`,
`ubos-mdns`, and `ubos-net`; neither depends on the other. They share
one listener on port 80 behind `ubos_http::PrefixRouter`, which
`ubos-firmware` wires.

`ubos-firmware` sits above everything above: it is the single library
the `firmware/<board>` binaries depend on, and the only crate that may
name the whole subsystem tier at once. The event-catalog registry and
the shared stratum session live there because each references all the
subsystem crates, which downward-only dependency edges forbid lower in
the graph. It is integration-level by nature — it pulls in
hardware-coupled crates such as `ubos-mining`, so it does not host-build
and carries no unit tests; anything worth testing stays in the
subsystem crates below. What cannot be hoisted stays in each board
`main.rs`: peripheral init and any `#[embassy_executor::task]` whose
signature names a concrete HAL type, because that attribute expands a
typed `TaskPool` static and so rejects generic task functions.

| Crate              | Purpose                                            |
|--------------------|----------------------------------------------------|
| `ubos-core`        | shared constants, watchdog feed loop, hold-to-trigger button, typed units, I2C bus clear, boot seeds struct |
| `ubos-led`         | the indicator LEDs: the status blink on a plain pin and the breathing RGB colour behind the `RgbFrame` trait, each with its task shell |
| `ubos-config`      | runtime `Config` watch, `ConfigHandle`, flash-backed `sequential-storage` persistence |
| `ubos-manager`     | supervisor crate: routes `Message::Halt`/`Resume` to mining/cooling/thermal handles |
| `ubos-net`         | DHCP, NTP sync; owns the cross-board atomics (IP, NTP offset, request count) and `wall_clock_secs` |
| `ubos-http`        | HTTP transport primitives: TCP accept loop, request parser, `Service` trait, `PrefixRouter`, the port 80 listener constants; generic response helpers (`read_body`, `write_json_response`, `MAX_RESPONSE_HEADERS`) |
| `ubos-auth`        | shared HTTP middleware: IP allow-list, credential store, session table, CSRF tokens, CSPRNG |
| `ubos-api`         | REST handlers, `REST_API_SERVICE` mDNS entry, schema types; serves `/api/*` on port 80 |
| `ubos-webui`       | Zopfli-gzipped `index.html`, `login.html` and `logo.svg`; dashboard handler and `WEBUI_SERVICE` mDNS entry on port 80 |
| `ubos-mdns`        | DNS-SD responder; per-service `declare_service!` macro composed by firmware |
| `ubos-metrics`     | Prometheus exporter task, hashrate EWMA, CPU-load gauge, shared metric map |
| `ubos-display`     | SH1106 OLED driver and render task                 |
| `ubos-logging`     | `defmt`/`log` macro facade                         |
| `ubos-thermal`     | `TemperatureSensor` and `McuTemperatureSensor` traits, descriptor-driven sensor fusion, sample loop, chip temperature gauge, MCU die temperature gauge, `ThermalHandle` publisher, thermal guard |
| `ubos-cooling`     | fan control task, PID or fixed duty, `FanOutput` HAL trait |
| `ubos-mining`      | BM13xx chip-serial actor, miner state machine, `MiningHandle` |
| `ubos-asic`        | pure chip-protocol library: the vendor-neutral `ChainDriver` seam plus the BM13xx chip family |
| `ubos-tuner`       | the tuner trait and the types it takes and returns. No loop and no tuner |
| `ubos-eeprom`      | hashboard EEPROM read trait and the BOS-compatible wire format |
| `ubos-events`      | user-facing event log: a fixed-capacity RAM ring of encoded records, readable over HTTP |
| `ubos-ota`         | board-agnostic OTA flash-write engine; each board supplies a backend |
| `mining-protocol`  | protocol-agnostic mining client trait and the stratum-to-chip boundary types |
| `stratum-client`   | integration layer between the protocol adapters and `AsicSession`: job ring, work ring, work generator |
| `stratum-v1`       | no_std Stratum V1 wire types, parser, and adapter |
| `stratum-v2`       | Stratum V2 adapter wrapping the vendored `micro-sv2` library |
| `esp32s3-tsens`    | ESP32-S3 on-die temperature sensor driver          |
| `rp235x-sha256`    | RP2350 hardware SHA-256 driver                     |
| `serial-baudrate`  | trait for runtime baud-rate changes on a serial line |
| `ubos-psu`         | async PSU driver over I2C                          |
| `ubos-temperature` | LM75A, TMP102, TMP461 and TMP464 drivers, the `Reading` every sensor yields, and the sensor descriptor types a board declares its topology with. No task and no metric |
| `ubos-machine`     | per-board `MachineDescriptor` consts composing the hashboard, PSU, and sensor descriptors with a control-board and platform descriptor; pure const data read by firmware and the host runner |
| `ubos-rp`          | per-platform support for the RP2040 and RP2350 control boards: the `#[panic_handler]` and `HardFault` symbols, reset diagnostic, `BoardWatchdog`, fan PWM and tachometer, `MinerUart`, the WS2812 adapter for the breathing status LED, the boot glue for the MAC, the seeds and the WIZnet bring-up, and the `spawn_miner_core1!` macro |
| `rp-bootloader`    | the `embassy-boot-rp` swap sequence shared by `rp2040-bootloader` and `rp2350-bootloader`; no `ubos` library above it |
| `ubos-stm32`       | per-platform support for the STM32F407 and STM32F439 control boards: fan PWM and tachometer, `MinerUart`, `BoardWatchdog`, and the Ethernet `Device` type with the `ethernet_device!` macro |
| `ubos-esp32`       | per-platform support for the ESP32-S3 control board: reset diagnostic, idle-accounting executor callbacks, the persistent event ring, `BoardWatchdog`, `MinerUart`, fan PWM and tachometer, the shared I2C bus aliases, the expander-driven reset pin, and the `spawn_miner_core1!` macro |
| `ubos-firmware`    | top-of-graph assembly: `events::install`, `net::bring_up`, shared stratum session, task-shell macros, the `serve_all!` listener macro; consumed only by the `firmware/<board>` binaries |

## `host/` — std crates

Crates that pull in the standard library and only build for the
developer host. No firmware image contains one. `ubos-build` is the only
entry that a firmware crate names, and it names it as a build
dependency. Cargo compiles a build script for the host, so `ubos-build`
is host code that a board reaches.

| Crate                  | Role                                          |
|------------------------|-----------------------------------------------|
| `host-runner`          | full miner loop running on Linux (TUN/TAP, AF_PACKET) |
| `ubos-main-host`       | empty stub for the planned TUN/TAP harness    |
| `ubos-build`           | build-script support for the board crates: linker wiring, the `memory.x` copy, and a build-id stamp |
| `embassy-net-afpacket` | Linux AF_PACKET driver for `embassy-net`      |
| `embedded-i2c-unix`    | `/dev/i2c-N` adapter for `embedded-hal-async` |
| `embedded-serial-unix` | Linux char-device serial for `embedded-io-async` |

## `firmware/` — per-target binaries

Each firmware crate is a binary that lives in `firmware/<board>/`.
Each contains its own `.cargo/config.toml` pinning the target triple
and the probe-rs runner. Builds and flashes must be invoked from
inside the crate directory so the per-crate config is picked up.

```
firmware/
├── bmm-pico/            thumbv8m.main-none-eabihf  (RP2350A, BMM adapter)
├── forge-axe/           thumbv8m.main-none-eabihf  (RP2350B, BM1370 hashboard)
├── esp32s3/             xtensa-esp32s3-none-elf    (ESP32-S3)
├── esp32s3-bootloader/  xtensa-esp32s3-none-elf
├── hashnode/            thumbv8m.main-none-eabihf  (RP2350B)
├── rp2040/              thumbv6m-none-eabi         (RP2040)
├── rp2040-bootloader/   thumbv6m-none-eabi
├── rp2350/              thumbv8m.main-none-eabihf  (RP2350A)
├── rp2350-bootloader/   thumbv8m.main-none-eabihf
├── stm32f407/           thumbv7em-none-eabihf      (STM32F407VGT6)
└── stm32f439/           thumbv7em-none-eabihf      (STM32F439ZIT6)
```

Firmware crate names are the board name, unprefixed. The directory
above the crate (`firmware/`) is the disambiguator. Per-board wiring,
pin assignments, and size budgets are in [BOARDS.md](BOARDS.md).

## `patches/` — upstream patches

Local patches against pinned upstream sources live here as `.patch`
files; the repo carries only the patches, never a vendored crate tree.

- Standalone patches applied to a nixpkgs or GitHub source by nix,
  outside any `[patch]` table. The tree carries none; `nix/probe-rs.nix`
  accepts one from a consumer outside this repo as `extraPatches`.
- `[patch]` override crates: a pinned pristine upstream source plus a
  patch (or ordered series) that `nix/patched-crates.nix` materializes
  at `patches/<crate>`, where the workspace `[patch]` table points. The
  dev shells symlink that tree into the working tree and
  `nix/source.nix` copies it into the check sources; the path itself is
  git-ignored. See `nix/patched-crates.nix`.

## `micro-sv2/` — vendored SV2 library

A self-contained nested Cargo workspace holding the Stratum V2 library: the
umbrella crate `ii-sv2` plus `ii-sv2-crypto`, `ii-sv2-framing`,
`ii-sv2-messages`, `ii-sv2-security`, and `ii-sv2-types`. It has its own
`[workspace]`, `Cargo.lock`, and `flake.nix` for standalone builds, and is
listed in the root workspace's `[workspace].exclude`. The root workspace
consumes it as an ordinary path dependency, `ii-sv2 = { path =
"micro-sv2/ii-sv2" }`, rather than as a member.

## `docs/` — long-form documentation

Cross-crate architecture notes, hardware references, and this
structure document. Rustdoc is the primary documentation: a fact about
one crate lives in that crate's `//!` docs, and a fact about one board
lives in that board's crate or in [BOARDS.md](BOARDS.md). Plans live
in `docs/plan/` and are deleted when their work lands. `README.md` and
`CLAUDE.md` stay at the workspace root because they are entry points
for humans and agents respectively.

## Conventions

- **One crate per directory; directory name = crate name.** This
  makes `Cargo.toml` reverse-dependency listings searchable by either
  the folder or the crate name, and makes renames mechanical.

- **Single workspace, single `Cargo.lock`.** No sub-workspaces, no
  per-crate flakes. Every crate is a member of the root workspace.

- **`[workspace.dependencies]` is the single source of truth for
  versions.** Member `Cargo.toml` files write `dep.workspace = true`
  with no version. Adding a new third-party dep means a one-line edit
  at the workspace root.

- **Per-crate `.cargo/config.toml` only for firmware crates.** It
  pins the target triple and the probe-rs runner; the workspace-wide
  `.cargo/config.toml` carries shared env vars only.

- **`ubos/` crates are `no_std`. `host/` crates use `std`.** The
  grouping enforces the boundary; a crate's location signals which
  target environment it builds for. A `host/` crate must never appear
  in a firmware crate's dep graph, and a `ubos/` crate must never
  pull in `std`.

## Footprint

Per-board RAM headroom after the multipool change (one stratum
session task per pool slot; the per-slot task future is ~15 KiB).
Numbers reflect a release build at the point this snapshot was
taken; Main RAM sums the `.data`/`.bss`/`.uninit` sections,
reproduce with the `cargo size` recipe below. A row marked "est." is
not measured. It is the last measurement plus the larger metrics render
buffer. Every row was measured with the fee switch off, three session
slots; a build that ships the fee slot adds one session task future
to Main RAM, so read the headroom against that. Each board states
the switch in its own `.cargo/config.toml`, and every board here
states `0`.

| Board     | Flash     | Main RAM   | CCM RAM      | Headroom note                                  |
|-----------|-----------|------------|--------------|------------------------------------------------|
| hashnode  | 576 / 2048 KiB | ~406 / 512 KiB (79%, est.) | — | comfortable; ~106 KiB free     |

The rows for the evaluation and adapter boards live in
[BOARDS.md](BOARDS.md).

`ubos-metrics`, `ubos-auth`, `ubos-api`, and `ubos-webui` expose a
`ccmram-bss` cargo feature that places non-DMA static state (metric
map, HTTP scratch buffers, credential / session / CSRF / CSPRNG
tables) in a `.ccmram.bss` section, for boards whose MCU carries a
Core-Coupled Memory region the linker otherwise leaves unused. No
board in this tree enables the feature, so the `link_section`
annotations are no-ops here; the boards that do, and their per-board
`memory.x`, are covered in [BOARDS.md](BOARDS.md). New CCM-eligible
statics should still carry:

```rust
#[cfg_attr(feature = "ccmram-bss", link_section = ".ccmram.bss")]
```

Each board states the size of the Prometheus exposition map as
`MAX_METRICS` and `METRICS_RESPONSE_BYTES` in its own `config.rs`, and
`ubos_firmware::serve_all!` declares the statics from there. Every board
in this tree holds 128 series, except `esp32s3`, `stm32f407` and
`stm32f439`, which hold 64. The RAM of these three boards does not hold
the larger map. On every board except the two STM32 boards, the buffer
has `METRIC_LINE_BYTES` for each series, so it holds a full map. The two
STM32 boards keep 4096 bytes.

Reproduce with:

```sh
nix develop .#arm -c bash -c 'cd firmware/<board> && cargo build --release && cargo size --release -- -A'
```

## Rationale

Four references shaped the layout. References are inline so the
reasoning is auditable.

### Grouped by build category, not flat

Matklad, *Large Rust Workspaces*
(<https://matklad.github.io/2021/08/22/large-rust-workspaces.html>),
argues for a single flat `crates/` directory for projects in the
10k–1M LoC range. For a general-purpose Rust project that argument
is decisive: one `ls` shows everything, new crates have one obvious
home, and the on-disk layout mirrors Cargo's flat namespace.

This project diverges from that recommendation for a specific
reason: every crate falls into exactly one of three categorically
distinct build environments, and the build commands differ between
them.

| Group       | `no_std`? | Build target                        | Build command                   |
|-------------|-----------|-------------------------------------|---------------------------------|
| `ubos/`     | yes       | host (clippy/test) or via firmware  | `cargo check -p <crate>`        |
| `host/`     | no        | host or `aarch64-linux` cross       | `cargo build -p <crate>`        |
| `firmware/` | yes       | per-crate target triple             | `cd firmware/<board> && cargo build` |

Mixing `host/` and `ubos/` crates as siblings would mean every new
crate prompts the same question: does it build with `std`? Does it
build under the workspace target or its own? The directory grouping
collapses that question into "where is the file?" — and prevents the
accidental dep that would, say, pull `host-runner` into a firmware
build.

`embassy-rs` ships each HAL family and each subsystem as a separately
versioned crate at the top of the namespace, diverging from matklad's
flat advice and succeeding. For a multi-target embedded workspace the
categorical split pays for itself in build-config clarity.

### Virtual manifest at root

Matklad, same reference:

> It might be tempting to put the main crate into the root, but that
> pollutes the root with `src/`, requires passing `--workspace` to
> every Cargo command, and adds an exception to an otherwise
> consistent structure.

Previously the workspace root was also a library crate. The dual role
conflated the directory name with the crate name
and forced `--workspace` on every cargo invocation. The five-crate
split of `src/` removes the need for a root crate entirely.

### Split `src/` along domain lines

Kashitsyn, *Rust at scale: packages, crates, and modules*
(<https://mmapped.blog/posts/03-rust-packages-crates-modules>):

- The unit of compilation is the crate, not the module. Modules do
  not buy incremental compilation; crates do.
- The trigger to split is dependency structure (hubs with many deps
  or reverse-deps), not LoC.
- API-level decoupling (generics, trait-associated types) should be
  exhausted before splitting.

Applied here: `metrics.rs` and `net.rs` are dependency hubs;
isolating them into their own crates keeps the hubs small and
stable. `display.rs` is a leaf with its own change
cadence. The split groups modules by change frequency, which matches
corrode.dev's pragmatic rule
(<https://corrode.dev/blog/tips-for-faster-rust-compile-times/>):

> Split when independent logical units change at different
> frequencies. Otherwise use feature flags.

### Embedded Rust precedent

Embassy (<https://embassy.dev/book/>) splits per-concern
(`embassy-net`, `embassy-time`, `embassy-executor`, `embassy-usb`,
`embassy-boot`) and per-MCU-family (`embassy-stm32`, `embassy-rp`,
`embassy-nrf`). The same instinct shapes the `ubos-*` namespace
here.

