#!/usr/bin/env bash
# Cargo runner that forwards to probe-rs-runner with an optional
# --probe selector taken from $PROBE_SERIAL. Bare `cargo run` leaves
# the variable unset and lets probe-rs auto-select the only attached
# probe; export PROBE_SERIAL (or use the `attach` alias) to pin a
# specific probe when several are plugged in.
#
# This indirection exists because cargo's `--config` flag merges
# array values from the CLI with those in config.toml rather than
# replacing them, so an alias that tried to set
# target.<triple>.runner=[...] ended up concatenating its arg list
# with the base runner's and feeding probe-rs two --chip flags.
set -euo pipefail

# Default to `run` (flash + reset + monitor). The `attach` alias sets
# $PROBE_RS_ACTION=attach so `cargo attach` connects to the already
# running core and streams defmt without flashing or resetting it. Both
# subcommands take the same --chip/--probe/--log-format/ELF arguments.
args=("${PROBE_RS_ACTION:-run}" --chip RP2040)
if [ -n "${PROBE_SERIAL:-}" ]; then
  args+=(--probe "$PROBE_SERIAL")
fi
log_format=(--log-format "{t:>10} {f:<14} [{L}] {s}")

# `cargo run` appends the freshly built ELF as the first positional arg.
# $PROBE_RS_ELF overrides it so `cargo attach` can decode defmt against
# a binary matching the firmware already on the board rather than the one
# this build just produced; mismatched symbol tables decode to garbage.
# Trailing args after `--` are forwarded either way.
if [ -n "${PROBE_RS_ELF:-}" ]; then
  shift || true
  args+=("${log_format[@]}" "$PROBE_RS_ELF" "$@")
else
  args+=("${log_format[@]}" "$@")
fi

exec probe-rs-runner "${args[@]}"
