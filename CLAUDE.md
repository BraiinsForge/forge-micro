# Firmware workspace

Open firmware for the ARM and Xtensa boards. Tree layout:
`docs/STRUCTURE.md`.

## Build

Every build goes through a `nix develop` shell, and **from the crate's
own directory** — the workspace root selects the wrong target and fails.
The `arm` shell covers the ARM targets, `esp` the Xtensa one.

```sh
nix develop .#arm -c bash -c 'cd firmware/rp2350  && cargo check'
nix develop .#esp -c bash -c 'cd firmware/esp32s3 && cargo check'
```

Which chip driver a board selects, and which cargo features are on by
default, is decided in `config.rs` rather than by a flag: see
`docs/ASIC.md` and `docs/SETUP.md`.

## Flash

Run from inside the crate directory. `cargo run --release` flashes over a
probe attached to this machine and auto-selects when a single probe is
plugged in; export `$PROBE_SERIAL` to pick one when several are, or pass
`--probe` after `--`. `probe-rs run` flashes then streams defmt logs, and
`cargo attach` streams a running board's logs without flashing or
resetting it, on the crates whose runner is `probe-rs-board-run`.

- To flash and detach, send **SIGINT** after the boot banner and the
  firmware keeps running.
- **Never run `probe-rs info` twice in a row on one probe.** That is
  what wedges a CMSIS-DAP probe, and recovery is a physical replug.

A silent `cargo run`, probe recovery, and size and stack analysis:
`docs/DEBUGGING.md`.

## Targets

Every firmware crate builds in the `arm` shell except `esp32s3` and
`esp32s3-bootloader`, which build in `esp`. The per-crate target
triples are in the firmware tree in `docs/STRUCTURE.md`.

## Verification

Every crate, library crates included, must be gated by the flake
`checks`: a `cargo build`/`cargo check` derivation and a
`cargo clippy -- -D warnings` one. **A crate that is not in `checks` is
not gated.** CI runs those checks as staged groups whose union is exactly
`nix flake check`. `nix run .#gate` builds all of them and prints a
table; it judges the committed tree and refuses a dirty one, and what it
refuses, what it covers and how to read the table is `docs/GATE.md`. The
same commands also run one at a time:

```sh
nix build .#ci-format                  # cargo fmt + nixfmt
nix build .#ci-clippy                  # per-crate clippy
nix build .#ci-check                   # builds, tests, static gates
nix build .#checks.<system>.<name>     # one check in isolation
nix flake check                        # everything
```

The `claude-md-limit` gate holds this file at or under 200 lines: it
loads on every session and adherence drops as it grows, so detail belongs
in `docs/` or in a path-scoped `.claude/rules/` file instead.

## Documentation

Rustdoc and nix comments are the primary documentation. A fact about
one crate lives in that crate's `//!` or `///` docs; a fact about one
board lives in that board's crate or in `docs/BOARDS.md`. `docs/` holds
only cross-crate architecture with a code-wins banner, hardware
references, and runbooks. Plans live in `docs/plan/` and are deleted
when their work lands.

## Licensing

GPL-3.0-or-later throughout, and public. Every new first-party source
carries the GPL block, and the `license-headers` check gates that rather
than trusting it. Which extensions are in scope, what the second half of
the block reserves and why it is load-bearing, which paths keep a licence
that is not ours, and what the manifests say:
`.claude/rules/licence-headers.md`, which loads with the sources it
governs.

Nothing here is confidential, so there is no strip step and no per-path
exception to reason about. Product-specific driver code and the bench
and lab machinery live in the internal repository that consumes this
one; a change that belongs there does not belong here.

## Role

Systems engineer. Deep expertise in Rust and embedded firmware.

## Principles

- **Simplicity above all**: minimal, correct code. No clever
  abstractions. When in doubt, write less.
- **Verify, don't trust**: test assumptions against the code and the
  docs. Build the affected crate after every change.
- **Push back**: question whether a solution is the simplest one, and
  disagree with a suboptimal approach rather than implementing it.

## Working Style

- Read the surrounding code before changing it, and match its patterns.
- Prefer editing an existing file over creating a new one.
- Comments are proper sentences, with no parenthetical asides and no
  size claims.
- New error messages follow `/simple-english`; existing ones are left
  alone until their meaning changes. Comments and commit messages keep
  the rules above and in `.gitlint` instead.
- Conventions live in `.claude/rules/`, path-scoped so each loads with
  the files it governs: `firmware.md` for the firmware and `ubos/`
  crates, `named-constants.md` and `branching.md` for every `.nix` and
  `.rs` file, and `prose.md` for every README and `docs/` file.
- **Never name internal prototypes, external miner projects, or chip
  datasheets** in code, comments, or commit messages. Each mention is a
  legal surface. Doc comments describe behavior, not lineage; a
  provenance question is answered in the review thread, not in the
  codebase.

## Model Routing

Opus orchestrates, sonnet executes. Delegate substantial work to sonnet
subagents; Opus keeps architecture, plan approval, coordination, and the
edits too small to be worth spawning for. Set `model: "sonnet"`
explicitly when spawning parallel agents.

## Git Workflow

Clarify → Plan → Implement → Commit → Review. **Wait for explicit user
approval before starting to implement, and never advance past a plan
without it.**

- Present findings and the proposed approach, then wait.
- Answer a question completely, then wait. A question is a question, not
  an implicit instruction.
- One logical change per iteration.
- Use plan mode for anything non-trivial.
- **Committing needs no approval.** A finished change is committed
  through `/commit` in the same turn that finishes it. Never leave
  finished work uncommitted, and never ask whether to commit it.
- **NEVER push to remote.** The user pushes when ready.

A branch runs plan → implement → `/review-branch` → `/fix-review` →
`/branch-cleanup`, iterated until no significant findings remain, then
human review, then the same loop again over the human's fixes.

## Commits and History

`gitlint` (`.gitlint` + `.gitlint-rules/`) is the enforced source of
truth for message format. The doctrine itself lives in the skills and
loads when they are invoked: `/commit` owns the message format, and
`/branch-cleanup` collapses a branch to small atomic commits with each
change touched once and no development archaeology.

Interactive rebase is allowed here, overriding the harness default.
Drive it non-interactively through `GIT_SEQUENCE_EDITOR`, and never set
`GIT_EDITOR`.

## Communication

- Direct, concise, technical. Lead with the answer.
- Structured *and* concise: let a heading, table, or list carry what a
  paragraph would bury.
- No preamble, no recap of work already shown, no restating the
  question, no summary of a summary.
- Report the conclusion and what backs it, not the path that reached it.
- A caveat is one line. Label it and move on: `Risk:`, `Assumption:`.
- No praise and no validation. Evaluate on technical merit only.
- No weasel words. Say "I don't know" when uncertain.
