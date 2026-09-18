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

# Cargo runner for the rp2040-bootloader crate: forwards to probe-rs-runner
# with an optional --probe selector. Packaged as a writeShellApplication so
# shellcheck and the probe-rs-runner PATH dep come for free. The rp2350
# bootloader has its own copy beside this one: the two differ in the chip
# they name, and a runner that took the chip as an argument would be a
# flag for cargo to thread through an alias, which is what the comment in
# run.sh explains cannot be done.
{
  writeShellApplication,
  probe-rs-runner,
}:
writeShellApplication {
  name = "rp2040-bootloader-run";
  runtimeInputs = [ probe-rs-runner ];
  text = builtins.readFile ./run.sh;
}
