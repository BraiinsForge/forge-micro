# The Flake Gate

How to run the gate for a review, and the ways of running it that have
failed. Read this before running the gate. A caller that supplies the
gate result does not need this file at all — its Phase 2 stands down and
nothing here applies.

## Contents

- Why one command, and which one
- Running it: in the background, with the exit line
- Two ways that have failed
- Reading the result
- What must never be substituted

## Why one command, and which one

```
nix run .#gate
```

This is the gate for a review. It builds every check of every tree of the
checkout and prints one table. It judges the committed tree, so it
refuses a tree that has uncommitted changes, and it names each failing
check as that check fails. `docs/GATE.md` holds what it refuses, what it
covers and how to read the table.

The checks it builds are the entire CI `check` stage, so one command
covers what the pipeline gates, and building the same derivations CI
builds is what stops the local result disagreeing with the pipeline.

Against `nix flake check .` the gate is the wider command: it builds the
checks of every submodule that carries a flake, and it runs in a pair
mid-branch, neither of which `nix flake check .` does. The one output it
does not reach is `nixosConfigurations`, and `docs/GATE.md` carries that
gap together with the rule that closes it.

Do not run per-flake `nix flake check <path>`; the root check already
includes them, and the root is the only place the nix-formatting check
lives.

It is a full hermetic build of every board. Warm — most reviews, since
the local `/nix/store` is already populated — it takes a minute or two;
cold it can run much longer.

The checks are hermetic in the strong sense: every toolchain, including
the Xtensa toolchain the `.#esp` shell uses, is supplied by the flake and
built by nix. So the gate runs every board's checks with no host setup,
the same way CI does. There is no "missing toolchain" to skip over; if a
check does not run, the environment is wrong — fix it and run the check.

## Running it: in the background, with the exit line

Run it as **one `Bash` call in the background**, with the tool's own
background mode and no trailing ampersand. `2>&1` is a redirection and
not a pipe, so the exit status stays the command's own:

```bash
nix run .#gate 2>&1
```

Add nothing else to that line. `.claude/hooks/gate-invocation` holds the
gate to its approved invocations and prints the whole rule when it
refuses one, so the list is not repeated here. There is nothing to
capture by hand: the gate prints its own verdict as the last line of the
run.

A background run overlaps the phases that follow instead of preceding
them, and it is the run's wall clock: a cold store takes it past the
longest foreground timeout, which used to end a review with the branch
ungated. Its completion notification is the whole of the wait, and it
re-enters this session on whatever turn it lands in, so the report is
emitted from that turn. `nix run .#gate -- status` reprints the newest
run's table from a later turn or a second terminal; `docs/GATE.md` says
how to read it and what it reports before there is a table.

## Two ways that have failed

Do not delegate the run to a `Task` subagent, and do not poll a log for
a completion sentinel. One invariant explains both failures: the
execution instance that reads the exit line must be the instance that
continues into Phase 3, and it reads that line from the notification
rather than from a loop.

- **`Task` subagent**: returns before the check finishes.
- **Hand-rolled `until grep …; do sleep; done` waiter**: spins whenever
  its sentinel never lands, and holds a shell for the whole run to learn
  nothing the notification would not have delivered.

## Reading the result

Read the last line of the output. It is `gate: PASS` or `gate: FAIL`.
On a failure, name each failing check and the error
lines that explain it. The gate's table already names them, and
`nix run .#gate -- log <check>` prints one check's build log.

If the run ends without that line, it was stopped before it could write
one. Record the result as `TIMED OUT (re-run manually)` and note that
the branch is not gated until it is re-run, rather than dropping the
report. Do not emit the final report until the line is in hand or the
timeout is recorded.

## What must never be substituted

Never substitute an ad-hoc `nixfmt`, `treefmt`, or per-crate
`cargo fmt --check`. Only a check built from the flake uses the formatter
version pinned in `flake.lock`, so only it matches CI exactly; a locally
installed formatter can be a different version and pass while CI fails.

`cargo check` and `cargo clippy` per changed crate are fine for fast
iteration, but a clean run does not substitute for the gate.
