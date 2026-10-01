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

# The commit linter behind `nix run .#gitlint`: every tree of the
# checkout, each against its own review base and under its own
# `.gitlint`, in one run with one exit status. It exists so a review
# lints the pair with one command instead of a hand-spelled
# `nix shell` per tree, and so the linter it runs is the one this
# flake's nixpkgs pins rather than whatever the flake registry
# resolves, which an allowlist proxy refuses.
#
# The trees are discovered from `.gitmodules`, the way the gate does
# it, and the root is resolved from the caller's directory, so the
# same command is correct from either half of a pair. A tree is judged
# only when it carries a `.gitlint`.
{ pkgs }:
let
  # What a tree is linted against when its branch names no
  # `reviewBase`. It is the remote master and not the local one,
  # which is often stale.
  defaultBase = "origin/master";

  gitlint = pkgs.writeShellApplication {
    name = "gitlint";
    runtimeInputs = [
      pkgs.coreutils
      pkgs.git
      pkgs.gitlint
    ];
    text = ''
      readonly default_base=${pkgs.lib.escapeShellArg defaultBase}
    ''
    + builtins.readFile ./gitlint.sh;
  };
in
{
  type = "app";
  meta.description = "Lint the branch commits of every tree against its review base";
  program = pkgs.lib.getExe gitlint;
}
