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

# Cargo runner shared by the probe-rs boards (bmm-pico, rp2040, rp2350):
# a writeShellApplication so shellcheck and the probe-rs-runner PATH dep
# come for free. Each crate's .cargo/config.toml sets PROBE_RS_CHIP in
# [env] and points its runner at this binary. Private to the internal
# tree, stripped from the mirror with the boards it serves.
{
  writeShellApplication,
  probe-rs-runner,
}:
writeShellApplication {
  name = "probe-rs-board-run";
  runtimeInputs = [ probe-rs-runner ];
  text = builtins.readFile ./run.sh;
}
