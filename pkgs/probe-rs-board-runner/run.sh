# Cargo runner shared by the probe-rs boards: forwards to probe-rs-runner
# with the chip from $PROBE_RS_CHIP (each crate's .cargo/config.toml [env]
# sets it) and an optional --probe from $PROBE_SERIAL. The run-N/attach-N
# aliases set those per board so a specific probe is picked when several
# are plugged in; bare `cargo run` leaves them unset for a single probe.
#
# This indirection exists because cargo's `--config` merges array values
# from the CLI with config.toml rather than replacing them, so an alias
# that set target.<triple>.runner=[...] concatenated its args with the
# base runner's and fed probe-rs two --chip flags.

chip="${PROBE_RS_CHIP:?PROBE_RS_CHIP must be set (see .cargo/config.toml [env])}"

# Default to `run` (flash + reset + monitor). The attach aliases set
# PROBE_RS_ACTION=attach so `cargo attach` streams defmt without flashing
# or resetting. Both take the same --chip/--probe/--log-format/ELF args.
args=("${PROBE_RS_ACTION:-run}" --chip "$chip")
if [ -n "${PROBE_SERIAL:-}" ]; then
  args+=(--probe "$PROBE_SERIAL")
fi
log_format=(--log-format "{t:>10} {f:<14} [{L}] {s}")

# `cargo run` appends the freshly built ELF as the first positional arg.
# PROBE_RS_ELF overrides it so `cargo attach` decodes defmt against the
# binary already on the board rather than this build's; mismatched symbol
# tables decode to garbage. Trailing args after `--` forward either way.
if [ -n "${PROBE_RS_ELF:-}" ]; then
  shift || true
  args+=("${log_format[@]}" "$PROBE_RS_ELF" "$@")
else
  args+=("${log_format[@]}" "$@")
fi

exec probe-rs-runner "${args[@]}"
