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

# The fee switch a build reads.
#
# ubos-config reads `DEVFEE` at build time and ships the fee slot when
# it is `1`. A board states the switch in the `[env]` of its own
# `.cargo/config.toml`, which cargo reads from the crate's directory
# and a nix build never sees, because crane runs cargo from the flake
# root. So a board's gate and image take the value from that file
# through `envFor`, and the one line a developer edits is the one the
# gates and the images follow. Every board in this tree states `0`,
# so every image here ships none and the pool stack runs the
# operator's slots alone. Every host gate takes its environment from
# here, so a check cannot leave the switch to the build host's shell.
let
  # The environment of a build that ships the fee slot.
  on = {
    DEVFEE = "1";
  };

  # The environment of a build that ships none, stated the way the
  # boards here state it.
  off = {
    DEVFEE = "0";
  };
in
{
  inherit on off;

  # The environment of a crate's build: the `DEVFEE` its cargo config
  # states, or nothing when the config states none. The value passes
  # through as written, so ubos-config judges it as it judges one
  # typed on a command. A table form, which cargo accepts for a path
  # value, is refused here by name, because a derivation would not
  # take it and the error would name nothing.
  envFor =
    crateDir:
    let
      file = crateDir + "/.cargo/config.toml";
      env = builtins.intersectAttrs { DEVFEE = null; } (
        (builtins.fromTOML (builtins.readFile file)).env or { }
      );
    in
    if env ? DEVFEE && !builtins.isString env.DEVFEE then
      throw "DEVFEE in ${toString file} must be a plain string, \"0\" or \"1\""
    else
      env;
}
