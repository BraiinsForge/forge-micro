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

# The workspace crates that each base crate is allowed to depend on,
# and the check that holds the manifests to that list.
#
# A base crate sits under most of the workspace, so one new edge from
# it to a crate above it can close a dependency cycle or pull a whole
# subsystem into every build. This file is the one place that states
# which edges exist, and a new edge is an edit here that a review sees.
#
# The check counts every dependency table of a manifest: dependencies,
# dev dependencies, build dependencies and the target tables. A dev
# edge counts because cargo then builds a second copy of the crate for
# the tests. A crate with no row here is not checked.
{
  pkgs,
  # The dependency walk over the manifests; see nix/crate-graph.nix.
  crateGraph,
}:
let
  inherit (pkgs) lib;

  allowed = {
    # The units and the shared primitives. Every crate can reach it.
    ubos-core = [ ];
    # The logging facade. Every crate can reach it.
    ubos-logging = [ ];
    # The event catalog logs each event that it records.
    ubos-events = [ "ubos-logging" ];
    # The config schema holds units, logs, and records its events.
    ubos-config = [
      "ubos-core"
      "ubos-events"
      "ubos-logging"
    ];
    # A hashboard descriptor holds units, the defaults of a config and
    # the sensors on its board. The tests compare a header hash with the
    # one mining-protocol computes, and that crate lists no workspace
    # crate.
    ubos-asic = [
      "mining-protocol"
      "ubos-config"
      "ubos-core"
      "ubos-temperature"
    ];
    # The metrics store holds the metric types, the store and the render.
    # A module that owns a state calculates it, holds it and pushes it,
    # and depends on this crate, so an edge back would close a cycle.
    ubos-metrics = [
      "ubos-core"
      "ubos-logging"
    ];
  };

  # Each `crate -> dependency` pair that a manifest lists and the row of
  # the crate does not allow.
  forbidden = lib.concatLists (
    lib.mapAttrsToList (
      crate: dependencies:
      map (dependency: "${crate} -> ${dependency}") (
        lib.subtractLists dependencies (crateGraph.directDepNames crate)
      )
    ) allowed
  );
in
pkgs.runCommand "crate-layers" { } ''
  ${lib.concatMapStrings (pair: ''
    echo "nix/crate-layers.nix does not allow the dependency ${pair}" >&2
  '') forbidden}
  test ${toString (builtins.length forbidden)} -eq 0
  touch $out
''
