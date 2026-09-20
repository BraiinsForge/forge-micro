# The Flake Gate

How to run the gate for a review, and the ways of running it that have
failed. Read this before running the gate. A caller that supplies the
gate result does not need this file at all — its Phase 2 stands down and
nothing here applies.

## Contents

- Why one command, and which one
- Running it: foreground, blocking, with the exit line
- Three ways that have failed
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

## Running it: foreground, blocking, with the exit line

Run it as **one foreground, blocking `Bash` call** with a long `timeout`
(the 600000 ms maximum). `2>&1` is a redirection and not a pipe, so the
exit status stays the command's own:

```bash
nix run .#gate 2>&1
```

Add nothing else to that line. `.claude/hooks/gate-invocation` holds the
gate to its approved invocations and prints the whole rule when it
refuses one, so the list is not repeated here. There is nothing to
capture by hand: the gate prints its own verdict as the last line of the
run.

A foreground call blocks the same turn until nix exits, so the result is
in hand before the call returns.

## Three ways that have failed

Do not background it, do not delegate it to a `Task` subagent, and do not
poll a log for a completion sentinel. Each has failed in a way rooted in
the review skill's forked context (`context: fork`): a fork is one-shot,
so once it returns its final message it cannot be re-entered.

- **Backgrounded**: the completion notification lands in the main loop,
  never in the finished fork, so the report gate is unreachable and the
  review ends with no report.
- **`Task` subagent**: returns before the check finishes.
- **Hand-rolled `until grep …; do sleep; done` waiter**: spins whenever
  its sentinel never lands.

## Reading the result

Read the last line of the output. It is `gate: PASS` or `gate: FAIL`.
On a failure, name each failing check and the error
lines that explain it. The gate's table already names them, and
`nix run .#gate -- log <check>` prints one check's build log.

If the call hits its timeout before that line appears, the store was
cold. Record the result as `TIMED OUT (re-run manually)` and note that
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
