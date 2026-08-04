# The Flake Gate

How to run `nix flake check .` as a review gate, and the ways of running
it that have failed. Read this before running the gate. A caller that
supplies the gate result does not need this file at all — its Phase 2
stands down and nothing here applies.

## Contents

- Why one command, and which one
- Running it: foreground, blocking, with the exit line
- Three ways that have failed
- Reading the result
- What must never be substituted

## Why one command, and which one

```
nix flake check .
```

The repo-root flake aggregates every project's checks (clippy, firmware
builds, host tests, DRC, ERC) under a `<project>-` prefix, plus the
repo-wide treefmt formatting check. This is the entire CI `check` stage
(`.gitlab/ci/lint.yml`), so one command covers what the pipeline gates,
and running it identically to CI is what stops the local result
disagreeing with the pipeline.

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
(the 600000 ms maximum). `2>&1` is a redirection, not a pipe, so `$?` is
nix's own exit status; capture it on the last line:

```bash
nix flake check . 2>&1; echo "FLAKE_CHECK_EXIT=$?"
```

A foreground call blocks the same turn until nix exits, so the exit
status is in hand before the call returns.

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

Read the last line of the output: PASS if it is `FLAKE_CHECK_EXIT=0`,
otherwise FAIL with each failing check name and the relevant error lines.

If the call hits its timeout before the exit line appears, the store was
cold. Record the flake check as `TIMED OUT (re-run manually)` and note
that the branch is not gated until it is re-run, rather than dropping the
report. Do not emit the final report until this exit line is in hand or
the timeout is recorded.

## What must never be substituted

Never substitute an ad-hoc `nixfmt`, `treefmt`, or per-crate
`cargo fmt --check`. Only the flake check uses the formatter version
pinned in `flake.lock`, so only it matches CI exactly; a locally
installed formatter can be a different version and pass while CI fails.

`cargo check` and `cargo clippy` per changed crate are fine for fast
iteration, but a clean run does not substitute for the gate.
