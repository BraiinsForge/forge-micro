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
nix develop .#arm -c bash -c 'cd firmware/hashnode    && cargo check'
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
nix develop .#arm -c bash -c 'cd firmware/hashnode  && cargo run --release'
nix develop .#arm -c bash -c 'cd firmware/stm32f407 && cargo run --release'
nix develop .#arm -c bash -c 'cd firmware/stm32f439 && cargo run --release'
nix develop .#esp -c bash -c 'cd firmware/esp32s3   && cargo run --release'
```

Every board drives a BM1362 hashboard via `ubos-mining` unconditionally,
except `hashnode`, which enables the `bm1370` feature to replay a BM1370
bring-up sequence instead. The `display` feature is always in defaults
and SV2 is always built on all boards.

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
| hashnode | thumbv8m.main-none-eabihf | RP2350B (Cortex-M33, 48 GPIO) |
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
  separate hand-managed executor for the `ubos-mining` serial worker;
  the busy-ratio metric is intentionally absent there.
- The esp32s3 firmware uses `#[esp_hal::main]` and constructs the
  esp-rtos embassy executor manually so it can pass idle hooks to
  `Executor::run_with_callbacks`.
- All boards share network tasks, constants, buffers, and response
  building via the `ubos-core`, `ubos-net`, and `ubos-metrics` crates
  under `ubos/`.
- Embassy task constructors return `Result<SpawnToken, SpawnError>`;
  `spawner.spawn()` returns `()`. The `unwrap!` wraps the constructor.
- ESP32 crate tracks `esp-rtos 0.3.0` / `esp-hal 1.1` and uses the
  workspace `embassy-executor 0.10`. esp-rtos 0.3.0 added stack-overflow
  detection (it panics `stack pointer out of range` instead of silently
  corrupting) and fixed the multi-core task-switch interrupt nesting that
  the 0.2.0 line corrupted under load.
- UART for the BM1362 chip serial link uses DMA on cortex-m targets:
  `embassy-stm32` `RingBufferedUartRx` for stm32f407/stm32f439, and
  `embassy-rp` `BufferedUart` (interrupt-driven, 1 KiB software ring
  buffer each direction) for rp2040/rp2350/bmm-pico. The RP hardware
  FIFO is 32 bytes; one-shot DMA would overflow it between `read()`
  calls at 921600 baud, and embassy-rp 0.10.0 ships no ring-buffered
  DMA RX. `BufferedUart` already implements `embedded_io_async::Read`
  and `Write` directly, so no newtype adapter is needed. The esp32s3
  board firmware stays on the interrupt+FIFO `Uart::into_async()` path.
  esp-hal 1.1.1 now ships the UHCI UART-over-DMA driver (`esp_hal::uart::uhci`,
  behind the `unstable` feature, which this crate enables), closing the
  esp-rs/esp-hal#2642 / #4720 gap that 1.0.0 had. UHCI RX supports idle-EOF
  framing, so it suits the request/response chip protocol, but switching is
  deferred: the 128-byte FIFO already wakes on a single RX byte, DMA only buys
  high-baud jitter headroom we have no evidence of needing, and the RX path
  must not be rewritten while it cannot be verified on hardware. All dual-core
  boards (rp2040, rp2350, bmm-pico, esp32s3) pin the
  ubos-mining `serial_task` to Core 1 so embassy-net on Core 0 stays
  responsive; the shared `ASIC_SESSION` uses `CriticalSectionRawMutex`,
  so cross-core sharing is free. On esp32s3, Core 1's executor also runs
  `IdleAccounting::<1>` so `IDLE_TICKS[1]` is populated for cpu-load
  metrics. The S3's 128-byte hardware FIFO absorbs the ETH-ISR-class
  jitter that overran the 1-byte USART holding register on stm32f407.

## Role

Systems engineer. Deep expertise in Rust and embedded firmware.

## Principles

- **Simplicity above all**: Minimal, correct code. No clever
  abstractions. When in doubt, write less.
- **Verify, don't trust**: Test assumptions through code and docs.
  Build the affected crate after every change (see Build).
- **Push back**: Be skeptical. Question whether the solution is truly
  simplest. Disagree on suboptimal approaches.

## Verification

Every crate, library crates included, must be covered by a build and a
clippy derivation in the flake `checks` output. Adding a crate means
adding a `cargo build`/`cargo check` and a `cargo clippy -- -D warnings`
derivation to `checks.<system>`; a crate that is not in `checks` is not
gated. CI runs the checks as staged groups whose union is exactly
`nix flake check`, and the same commands run locally — there is no
wrapper script:

```sh
nix build .#ci-format                  # cargo fmt + nixfmt
nix build .#ci-clippy                  # per-crate clippy
nix build .#ci-check                   # builds, tests, DRC/ERC, static gates
nix build .#checks.<system>.<name>     # one check in isolation
nix flake check                        # everything
```

## Working Style

- Read existing code patterns before making changes
- Use ripgrep/grep to understand the codebase
- Prefer editing existing files over creating new ones
- Keep responses concise and action-oriented
- Comments: proper sentences, no parenthetical asides, no size claims

**Safe commands** (run without confirmation):
- `cargo check`, `build`, `test`, `clippy`, `fmt` (inside the right
  `nix develop` shell)
- `git status`, `git diff`, `git log`, `git show`

**Configuration** (project-local, committed to the repo):
- `.claude/` for project-specific configuration and vendored skills
- `CLAUDE.md` for instructions

## Model Routing

**Opus orchestrates, sonnet executes.** Delegate substantial work to
sonnet subagents via the Task tool. Opus handles only architectural
decisions, plan approval, coordination, and 1-2 line edits where
spawning is slower.

When spawning multiple parallel agents, set `model: "sonnet"`
explicitly.

## Git Workflow

**Steps:** Clarify, Plan, Implement, Commit, Review. Fixups loop back
to Plan.

Wait for explicit user approval between steps. Never advance without it.

- Present findings and the proposed approach, then wait
- Answer questions completely first, then wait before acting
- One logical change per iteration, stop for user review
- Use plan mode for non-trivial tasks
- **NEVER push to remote** (the user pushes when ready)

## Commits and History

Commit and branch-history doctrine lives in the skills, which load only
when invoked. `gitlint` (`.gitlint` + `.gitlint-rules/`) is the enforced
source of truth for message format.

- `/commit` — atomic commits, chunk staging, and the authoritative
  message format
- `/branch-cleanup` — collapse a branch to small, logical, atomic
  commits: each change touched once, straight-linear history, no
  development archaeology; git-absorb (`git absorb --base "$BASE"`, the
  resolved review base) creates fixup commits, which this squashes before
  merge

Interactive rebase is allowed here (overriding the harness default
prohibition on `-i`). Drive it non-interactively via `GIT_SEQUENCE_EDITOR`;
never set `GIT_EDITOR`.

## Skills

| Skill | Purpose |
|-------|---------|
| `/commit` | Atomic commits with chunk-based staging |
| `/branch-cleanup` | Prepare a branch for merge with full history cleanup |
| `/review-branch` | Review all branch changes against master |
| `/fix-review` | Apply review findings as fixup commits |

## Communication

- Direct, concise, technical
- No praise or validation — evaluate on technical merit only
- No weasel words: avoid "likely", "probably", "might be"; say "I don't
  know" when uncertain
- Questions are questions: analyze and answer, do not treat as implicit
  instructions
