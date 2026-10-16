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

# The deterministic gate behind `nix run .#gate`. One command, one check
# set, one source, every time: every check of this tree and of every
# submodule that carries a flake, with no tier to pick and no diff to
# read. What varies between two runs is what the store already holds,
# and nothing else.
#
# Nothing here is configured. The trees to gate are discovered from
# `.gitmodules`, so a checkout with no submodule gates itself alone and
# one that carries a flake submodule gates both halves. That is what
# makes the same command correct in a standalone clone and in a clone
# that is the outer half of a pair, with no argument to remember and no
# second spelling to keep in step.
#
# Three properties are the whole design, and each costs something that a
# hand-typed `nix flake check` does not pay.
#
# Deterministic. The source is `path:` over a tree this script writes,
# not the flake's git fetcher and not the checkout. The git fetcher
# resolves a submodule from its remote whenever the outer tree is clean,
# so the same command judges the checkout on a dirty tree and the remote
# on a clean one. `path:` reads a directory either way, and `git archive`
# is what fills that directory: the tracked content of every tree and
# nothing else. An ignored build artifact therefore reaches no check,
# and the preflight refuses a tree that is not committed, so the content
# the gate judges is the content that merges.
#
# Observable. The build writes nix's own event stream to the state
# directory, and `gate status` renders it into one row per check. A
# second terminal reads that at any time, which is what makes a
# background run answerable rather than something to wait out.
#
# Parallel. The state directory hangs off the worktree's git directory,
# so two worktrees gating two branches never write to one file. Their
# builds meet in the store instead, where the daemon holds one lock per
# derivation: the second run waits on the first's identical derivations
# and then reuses the outputs. That is cache reuse rather than
# duplicated work, and it is why no lock here spans worktrees.
#
# What it covers, against `nix flake check`: every check of every gated
# tree, which is the whole of what that command builds, plus those
# trees' dev shells, which it evaluates. Each tree evaluates its checks
# and its dev shells in one job of its own, and the jobs run at once,
# so the fixed cost of a pair is about the cost of its slower half
# alone rather than the sum of both.
#
# The coverage is not identical, and the gap is one output.
# `nix flake check` also forces `nixosConfigurations`, and this
# evaluates `checks` and `devShells` and nothing else. A tree that
# carries a NixOS system therefore has to gate it from a check of its
# own, the way a `packages` output is gated, or the system is gated
# nowhere. That is the standing rule for any output this does not
# reach, and it is what keeps the two commands equivalent in practice
# rather than by accident.
{
  pkgs,
  # Baked rather than resolved at run time, so the flake attribute the
  # script names is the system it was built for.
  system,
}:
let
  # How many cores each derivation takes. The gate builds as many
  # derivations at once as the machine has cores, which it reads when it
  # runs. A check is one derivation per rustc unit, and most units use
  # one core for most of their build, so many jobs with few cores each
  # keep the machine busy where a few wide jobs leave it idle. Two
  # cores let the large units at the end of a graph, which build alone,
  # run faster. Both counts are overridable per run, because the
  # measured best pair differs between a build host and a laptop.
  coresPerJob = 2;

  # nix's own machine-readable event stream. Every line of it starts with
  # this, and `gate status` reads what follows.
  eventPrefix = "@nix ";

  # The activity type nix gives a build in that stream. A start carries
  # the derivation path, and the matching stop carries only the id.
  buildActivity = 105;

  # The activity type nix gives a download from a binary cache. A start
  # names the path and the cache, so the run can say which downloads
  # are open, which on a cold store is the whole of its first minutes.
  substituteActivity = 108;

  # The activity type of the one activity that counts every build of the
  # run. Its progress results carry the builds done and the builds nix
  # expects, which is the run's count of units out of its total.
  buildsActivity = 104;

  # How often the run reads the state of every check, and how long the
  # run's log can stay silent before it names what is still building.
  # The state costs a store query for every check, so a read per second
  # would cost more than it tells, and the log itself follows the event
  # stream at every poll.
  progressSeconds = 20;

  # How often the run tests whether the build has ended and prints the
  # events that landed since the last poll. It costs one signal and a
  # read of the stream from a cursor, so it is short.
  pollSeconds = 2;

  gate = pkgs.writeShellApplication {
    name = "gate";
    # `nix` and `nix-store` are deliberately absent: the gate has to
    # speak to the daemon the caller's own nix speaks to, so those two
    # come from the ambient PATH that `writeShellApplication` keeps in
    # front of nothing. Everything else the script runs is named here,
    # because a gate that calls itself deterministic cannot read its
    # own state machine with whichever awk the caller happens to have.
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
    # The script itself is nix/gate.sh. Nix owns eight values and
    # contributes nothing else, so the preamble below is the whole of
    # the boundary: the script reads each by name and states none of
    # them twice.
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
