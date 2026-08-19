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

# One-shot TAP + NAT setup for host-runner. Packaged as a
# writeShellApplication so shellcheck and its PATH deps come for free.
# `sudo` is deliberately left off runtimeInputs: it must be the system's
# setuid binary, not a nix-store copy, so the re-exec at the top of the
# script relies on it already being on the caller's PATH.
{
  writeShellApplication,
  iproute2,
  iptables,
  procps,
}:
writeShellApplication {
  name = "setup-network";
  runtimeInputs = [
    iproute2
    iptables
    procps
  ];
  text = builtins.readFile ./setup-network.sh;
}
