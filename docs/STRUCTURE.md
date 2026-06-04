# Repository Structure

Target layout for the `boards/ubos-main/` tree. This document is the
authoritative source for where new code, libraries, firmware crates,
patches, and assets belong. It exists because the previous layout
mixed workspace root, library crates, firmware binaries, host tools,
patches, and non-Rust assets at the top level with no naming
discipline; the rationale below records why the new shape was picked
and which authoritative references it is based on.

## Layout

```
boards/ubos-main/
├── Cargo.toml              virtual workspace manifest, no [package]
├── Cargo.lock
├── rust-toolchain.toml     pinned toolchain for the workspace
├── .cargo/config.toml      workspace-wide cargo config and env
├── flake.nix flake.lock    nix dev shells and flake checks
├── README.md               entry point, quick-start commands
├── CLAUDE.md               agent instructions, build conventions
├── docs/                   architecture, plans, this file
├── ubos/                   no_std library crates (firmware framework + drivers)
├── host/                   std library crates and host-side binaries
├── firmware/               per-target firmware binaries
├── nuc/                    NixOS configuration for the support host
├── pcb/                    KiCad hardware sources
└── patches/                vendored patches and forked crates
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
| `esp32s3-`, `rp235x-`, `serial-`      | hardware-specific drivers |

The firmware framework is split along domain lines. `ubos-core`,
`ubos-pool`, `ubos-logging`, and `ubos-config` are leaf crates with no
`ubos-*` dependencies above the logging facade; the chain
`(core + pool + logging + config) ← ubos-net ← ubos-metrics ← ubos-display`
runs strictly one-way above them. `ubos-manager` sits beside the chain
and supervises the thermal/cooling/mining handles via watch channels;
hardware and protocol crates sit beside the chain without participating
in it.

`ubos-auth` depends on `ubos-logging` only and holds the shared HTTP
middleware (allow-list, auth, sessions, CSRF, CSPRNG). Both listener
crates `ubos-webui` (port 80) and `ubos-api` (port 8080) depend on
`ubos-auth`, `ubos-http`, `ubos-mdns`, and `ubos-net`; neither depends
on the other.

| Crate              | Purpose                                            |
|--------------------|----------------------------------------------------|
| `ubos-core`        | shared constants, watchdog feed loop, status LED, typed units |
| `ubos-config`      | runtime `Config` watch, `ConfigHandle`, flash-backed `sequential-storage` persistence |
| `ubos-manager`     | supervisor crate: routes `Message::Halt`/`Resume` to mining/cooling/thermal handles |
| `ubos-net`         | DHCP, NTP sync, BTC fetch; owns the cross-board atomics (IP, BTC price, NTP offset, request count) and `wall_clock_secs` |
| `ubos-http`        | HTTP/1.0 transport primitives: TCP accept loop, request parser, `Service` trait; generic response helpers (`read_body`, `write_json_response`, `MAX_RESPONSE_HEADERS`) |
| `ubos-auth`        | shared HTTP middleware: IP allow-list, credential store, session table, CSRF tokens, CSPRNG |
| `ubos-api`         | REST handlers, `REST_API_SERVICE` mDNS entry, schema types; serves `/api/*` on port 8080 |
| `ubos-webui`       | gzipped `index.html` and `login.html`, raw `logo.svg`; webui listener (`run`) and `WEBUI_SERVICE` mDNS entry on port 80 |
| `ubos-mdns`        | DNS-SD responder; per-service `declare_service!` macro composed by firmware |
| `ubos-metrics`     | Prometheus exporter task, hashrate EWMA, CPU-load gauge, shared metric map |
| `ubos-display`     | SH1106 OLED driver and render task                 |
| `ubos-pool`        | Binance BTC ticker JSON parser                     |
| `ubos-logging`     | `defmt`/`log` macro facade                         |
| `ubos-thermal`     | sensor poll, `ThermalHandle` publisher, thermal guard |
| `ubos-cooling`     | fan PID controller and `FanOutput` HAL trait        |
| `ubos-mining`      | BM1362 chip-serial actor, miner state machine, `MiningHandle` |
| `ubos-psu`         | async PSU driver over I2C                          |
| `ubos-temperature` | temperature sampling and LM75A driver              |

## `host/` — std crates

Crates that pull in the standard library and only build for the
developer host or the NUC support node. They never appear in a
firmware crate's dep graph.

| Crate                  | Role                                          |
|------------------------|-----------------------------------------------|
| `host-runner`          | full miner loop running on Linux (TUN/TAP, AF_PACKET) |
| `ubos-main-host`       | thin host shim wrapping `stratum-client`      |
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
├── bmm-pico/      thumbv8m.main-none-eabihf  (RP2350)
├── rp2040/        thumbv6m-none-eabi         (RP2040)
├── rp2350/        thumbv8m.main-none-eabihf  (RP2350)
├── stm32f407/     thumbv7em-none-eabihf      (STM32F407)
├── stm32f439/     thumbv7em-none-eabihf      (STM32F439)
└── esp32s3/       xtensa-esp32s3-none-elf    (ESP32-S3)
```

Firmware crate names are the board name, unprefixed. The directory
above the crate (`firmware/`) is the disambiguator.

## `patches/` — vendored patches

All upstream patches and forked-crate sources live here.

- Single-file patches kept as `.patch` files (consumed by nix or by
  `[patch.crates-io]`).
- Vendored crate forks live in their own subdirectory and are wired
  into the workspace via `[patch.crates-io] crate = { path = "..." }`.

## `nuc/` — NixOS host configuration

Configuration for the support host (Raspberry Pi 4 today; the name
reflects the role, not the hardware). Owns systemd units, networking,
monitoring stack, and any host-specific Nix packages under `pkgs/`.
Renamed from `redacted/` because the hardware is no longer a constraint
of the role.

## `pcb/` — hardware sources

KiCad projects only. ERC and DRC are gated by flake `checks`.

## `docs/` — long-form documentation

Architecture notes, implementation plans, this structure document,
and any future ADR-style records. `README.md` and `CLAUDE.md` stay at
the workspace root because they are entry points for humans and
agents respectively; everything longer-form lives here.

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

Per-board RAM headroom after the API refactor. Numbers reflect a
release build at the point this snapshot was taken; reproduce with
the `cargo size` recipe below.

| Board     | Flash     | Main RAM   | CCM RAM      | Headroom note                                  |
|-----------|-----------|------------|--------------|------------------------------------------------|
| bmm-pico  | 458 / 2048 KiB | 242 / 512 KiB | —          | comfortable                                    |
| rp2040    | 478 / 2048 KiB | 240 / 256 KiB (94%) | —    | tight; ~16 KiB free, flag any new static state |
| rp2350    | 459 / 2048 KiB | 243 / 512 KiB | —          | comfortable                                    |
| stm32f407 | 459 / 1024 KiB | 126 / 128 KiB (98%) | 24 / 64 KiB CCM | tight; future statics must go to CCM        |
| stm32f439 | 460 / 2048 KiB | 174 / 192 KiB | 24 / 64 KiB CCM | comfortable                                |
| esp32s3   | 622 / 8192 KiB | ~249 / 320 KiB DRAM | — | comfortable                                    |

The stm32f4 firmwares ship per-board `memory.x` declaring the 64 KiB
Core-Coupled Memory region embassy-stm32 leaves unused. `ubos-metrics`,
`ubos-auth`, `ubos-api`, and `ubos-webui` expose a `ccmram-bss` cargo
feature that places non-DMA static state (metric map, HTTP scratch
buffers, credential / session / CSRF / CSPRNG tables) in
`.ccmram.bss`. Only stm32f407 and stm32f439 enable the feature; on
every other board the `link_section` annotations are no-ops. New
CCM-eligible statics should carry:

```rust
#[cfg_attr(feature = "ccmram-bss", link_section = ".ccmram.bss")]
```

Reproduce with:

```sh
nix develop .#arm -c bash -c 'cd firmware/<board> && cargo build --release && cargo size --release -- -A'
nix develop .#esp -c bash -c 'cd firmware/esp32s3 && cargo build --release'
nix develop .#esp -c bash -c 'xtensa-esp-elf-size -A \
  /home/jkr/projects/rusty-boards-api/build/target/xtensa-esp32s3-none-elf/release/esp32s3'
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

The upstream `bos-main` mining stack (~70 crates) uses subsystem
grouping (`open/bosminer/`, `open/boser/`, `open/utils-rs/`);
`embassy-rs` ships each HAL family and each subsystem as a separately
versioned crate at the top of the namespace. Both diverge from matklad's flat advice and both succeed.
For a multi-target embedded workspace the categorical split pays for
itself in build-config clarity.

### Virtual manifest at root

Matklad, same reference:

> It might be tempting to put the main crate into the root, but that
> pollutes the root with `src/`, requires passing `--workspace` to
> every Cargo command, and adds an exception to an otherwise
> consistent structure.

Previously `ubos-main` was both the workspace root and a library
crate. The dual role conflated the directory name with the crate name
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
stable. `display.rs` and `btc.rs` are leaves with their own change
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

reference-miner (<https://github.com/example/reference-miner>) splits its C
codebase into `components/` (6 reusable libraries) and `main/` (6
app-layer subdirectories plus 30+ source files). The Rust analogue
maps cleanly: their `components/` is our `ubos/` + `host/`, their
`main/` is each firmware crate's `src/`. Their granularity is
similar to what this project targets at growth.

## Migration plan

> **Status.** Complete. The reorganization landed across the commits
> described below; this section is kept as a historical record of the
> sequence.

The reorganization landed across a sequence of atomic commits, each
keeping `cargo check` green for every affected crate:

1. `redacted/` → `nuc/` (independent rename).
2. `firmware/embassy-net-wiznet-patch/` → `patches/embassy-net-wiznet/`
   with the `[patch.crates-io]` path updated.
3. Every existing `no_std` library crate moves from the workspace
   root into `ubos/`; workspace members and dependencies paths updated
   in lockstep.
4. Every existing host crate moves into `host/`.
5. Root `Cargo.toml` converts to a virtual manifest; the old root
   library crate moves to `ubos/ubos-main/` as an intermediate step.
6. `ubos-main` splits into five crates along domain lines: `ubos-pool`,
   `ubos-display`, `ubos-core`, and finally `ubos-net` plus
   `ubos-metrics` together. The `run_http_server` task moves out of
   `net.rs` into `http.rs` during the final split to break the
   net→http back-reference.
7. The empty `ubos-main` shell is deleted at the end of the split.
