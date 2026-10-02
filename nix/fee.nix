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

# The fee switch a build reads. A board states `DEVFEE` in the `[env]` of
# its own `.cargo/config.toml`, which a nix build never sees, because
# crane runs cargo from the flake root. So a board's gate and image read
# it from that file through `envFor`, and every host gate takes its
# environment from here, so no check leaves the switch to the build
# host's shell.
let
  on = {
    DEVFEE = "1";
  };

  off = {
    DEVFEE = "0";
  };
in
{
  inherit on off;

  # The `DEVFEE` a crate's cargo config states, passed through as written
  # so ubos-config judges it as one typed on a command. A table form,
  # which cargo accepts, is refused by name, because a derivation would
  # not take it and the error would name nothing.
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
