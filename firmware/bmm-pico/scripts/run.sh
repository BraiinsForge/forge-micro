#!/usr/bin/env bash
# Cargo runner that forwards to probe-rs-runner with an optional
# --probe selector taken from $PROBE_SERIAL. The `run-N` aliases in
# .cargo/config.toml set this variable per board so a specific probe
# is picked when several are plugged in; bare `cargo run` leaves it
# unset and lets probe-rs auto-select the only attached probe.
#
# This indirection exists because cargo's `--config` flag merges
# array values from the CLI with those in config.toml rather than
# replacing them, so an alias that tried to set
# target.<triple>.runner=[...] ended up concatenating its arg list
# with the base runner's and feeding probe-rs two --chip flags.
set -euo pipefail

args=(run --chip RP235x)
if [ -n "${PROBE_SERIAL:-}" ]; then
  args+=(--probe "$PROBE_SERIAL")
fi
args+=(--log-format "{t:>10} {f:<14} [{L}] {s}" "$@")

exec probe-rs-runner "${args[@]}"
