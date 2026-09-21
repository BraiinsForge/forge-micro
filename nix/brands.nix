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

# Which brand each board ships.
#
# Every board embeds ubos-webui, whose build.rs reads the brand from the
# directory `UBOS_BRAND` names. A board declares that directory in its
# own `.cargo/config.toml`, but a nix build never reads such a file:
# crane runs cargo from the flake root, which is not a descendant of the
# crate directory, so cargo merges no config from it. This table is what
# a nix build reads instead, and `brand-declaration` in nix/checks.nix
# gates the two against each other.
#
# `prefix` is where the brand directories sit relative to `root`. Both
# directories live in the open tree, because both trees build boards
# that ship them, so the internal repository passes the submodule
# prefix and no directory is duplicated.
{
  lib,
  root,
  prefix ? "",
}:
let
  # The boards that ship Forge Firmware. Every other board ships uBOS,
  # which is the whole of the internal repository and the rest of this
  # tree. brand/README.md carries the same table in prose.
  forgeBoards = [
    "rp2040"
    "hashnode"
    "forge-axe"
  ];

  forge = "brand/forge";
  ubos = "brand/ubos";
in
rec {
  # Re-exported so a consumer that builds a message about these files
  # can spell their paths the way the calling tree does. nix/brand-gates.nix
  # is the one reader.
  inherit prefix;

  # The brand directory a board ships, relative to `root`.
  dirFor = name: prefix + (if lib.elem name forgeBoards then forge else ubos);

  # The build environment that states a board's brand. Interpolating the
  # path copies that one directory into the store, so a build depends on
  # the brand it ships and on no other.
  envFor = name: { UBOS_BRAND = "${root + "/${dirFor name}"}"; };

  # The same, for a build that compiles ubos-webui without being a board:
  # a host gate of the crate itself, or a bootloader that pulls it into
  # its closure. Such a build ships no pages, so the brand it gets is
  # arbitrary and uBOS is the one every non-Forge board already uses.
  # Named rather than reached through `envFor`, so a board name that no
  # table row matches cannot be mistaken for one of these.
  defaultEnv = {
    UBOS_BRAND = "${root + "/${prefix + ubos}"}";
  };

  # The brand name a board's pages carry, read from the same file the
  # build reads so nothing restates it.
  nameFor = name: (builtins.fromJSON (builtins.readFile (root + "/${dirFor name}/brand.json"))).name;

  # Every brand directory in use, for the gate that checks each one is
  # complete.
  allDirs = map (dir: prefix + dir) [
    forge
    ubos
  ];
}
