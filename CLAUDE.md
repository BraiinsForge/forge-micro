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
except `hashnode`, whose board descriptor selects the 4-chip BM1370
instead (there is no cargo feature for it). The `display` feature is
always in defaults and SV2 is always built on all boards.

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

Firmware and `ubos/` implementation conventions load automatically from
`.claude/rules/firmware.md` when working under those trees.

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

## Licensing

The published mirror is GPL-3.0-or-later; see `LICENSE` and the README
License section. Header conventions for new files:

- First-party published sources (`.rs`, `.nix`, `.py`, `.js`, `.sh`,
  `.css`, `.html`) carry the GPL-3.0-or-later block; copy it from a
  neighbouring file. Prose header only, no SPDX identifiers.
- Sources in the private board crates, every `firmware/` crate but
  `hashnode` and `rp2350-bootloader`, carry the proprietary
  all-rights-reserved notice instead; those crates are stripped from
  the mirror.
- Vendored `patches/*` trees keep their upstream license and get no
  Braiins header.
- Private sources outside the board crates that the mirror strips
  (`publish/`, `nix/private-*.nix`, `micro-sv2/interop-test`, the
  redacted trees, `nuc/`, and the bench tooling modules listed at
  `stripPaths` in `publish/app.nix`) carry no header; the strip
  itself keeps them private.

Which crates are stripped, and why `rp2350-bootloader` stays public, is
documented at `stripPaths` and `HIDDEN_MEMBERS` in `publish/app.nix`.

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

**Branch lifecycle** — the preferred path from request to merge:

1. Gather the requirements for what the branch should implement.
2. Prepare a plan; once approved, commit it.
3. Implement all the changes and features.
4. Iterate `/review-branch` → `/fix-review` → `/branch-cleanup` until no
   significant findings remain.
5. Human review is expected at this point.
6. After the human's fixes land, run review → fix → cleanup again.
7. A clean branch with no significant findings is ready to merge.

**Per step:** Clarify, Plan, Implement, Commit, Review. Wait for explicit
user approval between steps; never advance without it.

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
