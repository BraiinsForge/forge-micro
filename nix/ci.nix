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

# CI stage groups. Each is a linkFarm over a slice of the flake's
# `checks`, so `nix build .#ci-<stage>` builds exactly that slice and
# nothing else - the CI pipeline runs these and a developer runs the
# identical command locally. No shell wrapper sits between CI and nix.
#
# `classify` maps every check name to exactly one stage, so the
# three groups partition `checks` with no overlap and no gaps:
# the union of ci-format + ci-clippy + ci-check is precisely
# `nix flake check`. ci-format is the formatting gate, ci-clippy
# the per-crate clippy gate, and ci-check the comprehensive
# remainder - host tests, firmware release builds, and the static
# guards. A newly added check that matches neither the format set
# nor the `clippy-` prefix falls into ci-check, so coverage can
# never silently shrink. Run a single check directly with
# `nix build .#checks.<system>.<name>`.
{ pkgs, checks }:
let
  classify =
    name:
    if
      builtins.elem name [
        "fmt"
        "fmt-load-test"
        "nixfmt"
        "micro-sv2-formatting"
      ]
    then
      "ci-format"
    else if pkgs.lib.hasPrefix "clippy-" name then
      "ci-clippy"
    else
      "ci-check";
  grouped = pkgs.lib.groupBy classify (builtins.attrNames checks);
  farm = stage: pkgs.linkFarmFromDrvs stage (map (name: checks.${name}) (grouped.${stage} or [ ]));
in
{
  ci-format = farm "ci-format";
  ci-clippy = farm "ci-clippy";
  ci-check = farm "ci-check";
}
