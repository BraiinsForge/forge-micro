# Copyright (C) 2026  Braiins Systems s.r.o.
#
# This program is free software: you can redistribute it and/or modify
# it under the terms of the GNU General Public License as published by
# the Free Software Foundation, either version 3 of the License, or
# (at your option) any later version.
#
# This program is distributed in the hope that it will be useful,
# but WITHOUT ANY WARRANTY; without even the implied warranty of
# MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
# GNU General Public License for more details.
#
# You should have received a copy of the GNU General Public License
# along with this program.  If not, see <https://www.gnu.org/licenses/>.
#
# Braiins Systems s.r.o. reserves the right to grant any party a license
# to this program, or any part thereof, under any terms, and such a grant
# shall be considered distinct from the grant above.


# Cargo runner that forwards to probe-rs-runner with an optional
# --probe selector taken from $PROBE_SERIAL. Export it to pick one
# probe when several are plugged in; bare `cargo run` leaves it unset,
# which auto-selects the only attached probe.
#
# This indirection exists because cargo's `--config` flag merges
# array values from the CLI with those in config.toml rather than
# replacing them, so an alias that tried to set
# target.<triple>.runner=[...] ended up concatenating its arg list
# with the base runner's and feeding probe-rs two --chip flags.

# `run` flashes + resets + attaches. The bootloader emits no defmt and
# hands off to the ACTIVE app within milliseconds, so nothing streams;
# send SIGINT after the flash completes to detach. The action is kept as
# `run` rather than `download` so the flash is symmetric with the
# application crates.
#
# The bootloader is flashed once per board, before the application ever
# runs: the application links into the ACTIVE partition and the bootrom
# reaches it only through the boot2 and the bootloader this crate
# carries. A board that has only ever had the application written to it
# boots whatever stands at the bootloader's place instead.
args=(run --chip RP2040)
if [ -n "${PROBE_SERIAL:-}" ]; then
  args+=(--probe "$PROBE_SERIAL")
fi
log_format=(--log-format "{t:>10} {f:<14} [{L}] {s}")

# `cargo run` appends the freshly built ELF as the first positional arg.
# Trailing args after `--` are forwarded to probe-rs, `--probe` among
# them for a host with more than one probe wired to it.
args+=("${log_format[@]}" "$@")

exec probe-rs-runner "${args[@]}"
