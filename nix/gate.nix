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

# The wrapper around nix/gate.sh, which is the gate behind
# `nix run .#gate`. docs/GATE.md holds what the gate covers, what it
# refuses, how to read its table and the rule for an output it does
# not reach; the reasons behind its steps are in the script.
#
# Nothing here is configured. The trees to gate are discovered from
# `.gitmodules`, so one spelling of the command is correct in a
# standalone clone and in a clone that is the outer half of a pair.
{
  pkgs,
  # Baked rather than resolved at run time, so the flake attribute the
  # script names is the system it was built for.
  system,
}:
let
  # How many cores each derivation takes; the gate runs as many
  # derivations at once as the machine has cores. Most rustc units use
  # one core for most of their build, so many narrow jobs keep the
  # machine busy where a few wide ones leave it idle, and two cores speed
  # the large units that end a graph alone. Both counts are overridable
  # per run, because the best pair differs between a build host and a
  # laptop.
  coresPerJob = 2;

  # Every line of nix's machine-readable event stream starts with this.
  eventPrefix = "@nix ";

  # The activity type nix gives a build in that stream.
  buildActivity = 105;

  # The activity type nix gives a download from a binary cache. A start
  # names the path and the cache, so the run can say which downloads are
  # open, which on a cold store is the whole of its first minutes.
  substituteActivity = 108;

  # The activity type that counts every build of the run: its progress
  # results carry the builds done and the builds nix expects.
  buildsActivity = 104;

  # How often the run reads the state of every check, and how long its
  # log can stay silent before it names what is still building. Each
  # read queries the store for every check, so a read per second would
  # cost more than it tells.
  progressSeconds = 20;

  # How often the run tests whether the build has ended and prints the
  # events that landed since the last poll. It costs one signal and a
  # read of the stream from a cursor, so it is short.
  pollSeconds = 2;

  gate = pkgs.writeShellApplication {
    name = "gate";
    # `nix` and `nix-store` are absent because the gate has to speak to
    # the daemon the caller's own nix speaks to, so those two come from
    # the ambient PATH. Everything else is named here, because a gate
    # that calls itself deterministic cannot read its own state machine
    # with whichever awk the caller happens to have.
    runtimeInputs = [
      pkgs.coreutils
      pkgs.findutils
      pkgs.gawk
      pkgs.git
      pkgs.gnugrep
      pkgs.gnused
      pkgs.gnutar
      pkgs.jq
    ];
    # The preamble is the whole of the boundary: nix owns these eight
    # values and contributes nothing else to nix/gate.sh.
    text = ''
      readonly cores_per_job=${toString coresPerJob}
      readonly event_prefix=${pkgs.lib.escapeShellArg eventPrefix}
      readonly build_activity=${toString buildActivity}
      readonly substitute_activity=${toString substituteActivity}
      readonly builds_activity=${toString buildsActivity}
      readonly progress_seconds=${toString progressSeconds}
      readonly poll_seconds=${toString pollSeconds}
      readonly nix_system=${pkgs.lib.escapeShellArg system}
    ''
    + builtins.readFile ./gate.sh;
  };
in
{
  type = "app";
  meta.description = "Run every check of every tree, and report one table";
  program = pkgs.lib.getExe gate;
}
