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

# The gate on the paths instruction files name; the rule is in
# instruction-paths.sh. Both trees of the pair run it, so it lives here
# and each tree calls it with its own root, the way nix/brand-gates.nix
# is shared: two copies of one rule drift.
{
  pkgs,
  # The tree whose CLAUDE.md and .claude are scanned.
  root,
  # The fewest instruction files the scan must reach, so that a scan
  # that reads nothing fails rather than passes.
  minFiles,
  # Top-level names a token may start with although no directory here
  # carries them: what a consumer tree reaches through the submodule,
  # or a mirror its agent definitions read.
  skipTops ? [ ],
}:
let
  scan = pkgs.writeShellApplication {
    name = "instruction-paths";
    runtimeInputs = with pkgs; [
      coreutils
      findutils
      gnugrep
      gnused
    ];
    text = builtins.readFile ./instruction-paths.sh;
  };
in
pkgs.runCommand "instruction-paths" { } ''
  cd ${root}
  ${scan}/bin/instruction-paths ${toString minFiles} ${pkgs.lib.escapeShellArgs skipTops}
  touch $out
''
