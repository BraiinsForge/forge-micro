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

# The board-skeleton parity gate: a board crate must take its task
# shells from the ubos crates that own them, and its boot sequence from
# ubos-firmware, rather than write a copy. Nothing else asks that
# question, so three boards have already arrived on master carrying
# twenty hand-written shells each and passed every check in both trees.
#
# The gate has two halves, because either one alone leaves the same hole
# those boards went through:
#
#   * A negative half over `#[embassy_executor::task]`. The name a board
#     declares must be in `fleetShells` or in that board's own `holders`
#     entry. This is what catches a hoisted shell written out by hand.
#   * A positive half over `requiredCalls`. A board's own copy of the
#     boot sequence carries no attribute at all, so the negative half
#     cannot see it; what it does have is the absence of the three calls
#     every board that uses the shared sequence makes.
#
# Both trees of the pair import this module. The shared names live here
# in one copy, and a call site passes only its own boards and their
# exceptions, so neither tree learns the other's board names.
{ pkgs }:
let
  inherit (pkgs) lib;

  # The task shells a board may still declare by hand. Every other task
  # in a board crate must expand from a macro in the crate that owns the
  # loop it calls.
  #
  # `boot` is the composition root itself. `mdns_task` and
  # `cpu_load_task` name per-board consts, and `stratum_client_task`
  # needs the board's own session type and pool size. docs/MACHINE.md
  # carries this list in prose.
  fleetShells = [
    "boot"
    "cpu_load_task"
    "mdns_task"
    "stratum_client_task"
  ];

  # The three calls that reach the hoisted boot sequence. A board that
  # names all three took the shared network bring-up, the shared
  # listener set and the shared event registry; a board that misses one
  # has its own copy of that region.
  requiredCalls = [
    "ubos_firmware::events::install"
    "ubos_firmware::net::bring_up"
    "ubos_firmware::serve_all!"
  ];

  # A Rust line comment, for both halves of the gate: the task scan below
  # skips such a line, and the call search ignores one. One binding, so
  # the two halves cannot come to disagree on what a comment is.
  commentLine = "^[[:space:]]*//";

  # The board crates of one tree: every directory under its `firmware/`
  # less `exclude`. Both gates below take their boards from here rather
  # than from a list written out, because an explicit list is what lets
  # the next board arrive ungated.
  boardsOf =
    root: exclude:
    let
      entries = builtins.readDir (root + "/firmware");
    in
    lib.naturalSort (
      lib.filter (name: entries.${name} == "directory" && !(builtins.elem name exclude)) (
        builtins.attrNames entries
      )
    );

  # An awk program over one board's sources. It prints `file:line name`
  # for every task the crate declares itself.
  #
  # A comment line is skipped before anything else, because ten boards
  # quote the attribute inside the doc comment on `stratum_client_task`
  # and the gate would fail on all ten.
  #
  # After the attribute, further attributes and blank lines are allowed
  # before the function: forge-axe writes its `boot` attribute above a
  # comment block and an `#[allow]`. Any other line ends the search, so
  # an attribute that names no task cannot attach itself to a function
  # further down the file.
  scanProgram = ''
    $0 ~ "${commentLine}" { next }
    /#\[embassy_executor::task/ { pending = 1; next }
    !pending { next }
    /async fn / {
      name = $0
      sub(/.*async fn /, "", name)
      sub(/[^A-Za-z0-9_].*/, "", name)
      print FILENAME ":" FNR, name
      pending = 0
      next
    }
    /^[[:space:]]*#\[/ { next }
    /^[[:space:]]*$/ { next }
    { pending = 0 }
  '';
in
{
  # Emit the check for one tree. `root` is that tree's own root, and the
  # board list is `boardsOf` it. `holders` maps a board name to the tasks
  # that are genuinely its own.
  boardSkeletonParity =
    {
      root,
      exclude ? [ ],
      holders ? { },
    }:
    let
      firmwareDir = root + "/firmware";
      boards = boardsOf root exclude;
      boardCall =
        board:
        "check_board ${board} '${lib.concatStringsSep " " (fleetShells ++ (holders.${board} or [ ]))}'\n";
    in
    pkgs.runCommand "board-skeleton-parity" { } (
      ''
        # The store copy of the tree's firmware/ is the working
        # directory, so the file names awk prints are relative and read
        # as repository paths.
        cd ${firmwareDir}

        status=0

        # True when a line that is not a comment names the literal in the
        # board's sources. A board that writes its own boot sequence and
        # names the shared call in a comment beside it would otherwise
        # pass on that comment, which is the evidence this half of the
        # gate rests on. The second grep is not -q, because under
        # pipefail a reader that stops early fails the pipeline it was
        # satisfied by.
        names() {
          [ -n "$(grep -rhF -- "$1" "$2/src" | grep -v ${lib.escapeShellArg commentLine})" ]
        }

        check_board() {
          board="$1"
          allowed="$2"

          if [ ! -d "$board/src" ]; then
            echo "ERROR: firmware/$board has no src directory." >&2
            echo "       Every directory under firmware/ is a board crate, unless the" >&2
            echo "       call site in nix/checks.nix excludes it." >&2
            status=1
            return
          fi

          # -r keeps awk from reading stdin if a board has no sources at
          # all, which would hang the build rather than fail it.
          find "$board/src" -name '*.rs' -print0 |
            xargs -0 -r awk ${lib.escapeShellArg scanProgram} > "$TMPDIR/tasks"

          while read -r loc name; do
            case " $allowed " in
              *" $name "*) continue ;;
            esac
            echo "ERROR: $board declares the task $name at $loc." >&2
            echo "       A board must not write a hoisted task shell by hand. Take the" >&2
            echo "       shell from the macro in the crate that owns the loop it calls." >&2
            echo "       If the task is this board's own, add its name to the board's" >&2
            echo "       holders in nix/checks.nix." >&2
            status=1
          done < "$TMPDIR/tasks"

          for call in ${lib.concatMapStringsSep " " lib.escapeShellArg requiredCalls}; do
            if ! names "$call" "$board"; then
              echo "ERROR: $board does not name $call." >&2
              echo "       A board must take the boot sequence from ubos-firmware. A" >&2
              echo "       board that writes its own copy passes every other check." >&2
              status=1
            fi
          done

          echo "board-skeleton-parity: $board declares $(wc -l < "$TMPDIR/tasks") tasks"
        }

      ''
      + lib.concatMapStrings boardCall boards
      + ''

        if [ "$status" -ne 0 ]; then
          exit 1
        fi
        echo "board-skeleton-parity: ${toString (builtins.length boards)} boards checked"
        touch $out
      ''
    );

  # Every firmware crate reads its target, its runner and its defmt
  # filter from its own .cargo/config.toml, and a crate that loses the
  # file builds for the host and reaches no probe. No build check
  # notices: crane runs cargo from the workspace root and supplies the
  # target itself. The tree's .gitignore ignores every .cargo/ directory
  # and then names the firmware pair back in, so the file is tracked
  # without a force-add, and the flake source carries tracked files only:
  # a new board's untracked copy fails here as a missing one. `exclude`
  # names the crates that build through a shell's own configuration
  # rather than a runner of their own.
  firmwareCargoConfig =
    {
      root,
      exclude ? [ ],
    }:
    pkgs.runCommand "firmware-cargo-config" { } ''
      for crate in ${root + "/firmware"}/*/; do
        case "$(basename "$crate")" in
          ${lib.concatMapStrings (name: "${name}) continue ;;\n          ") exclude}*) ;;
        esac
        if [ ! -f "$crate.cargo/config.toml" ]; then
          echo "ERROR: firmware/$(basename "$crate") has no tracked .cargo/config.toml." >&2
          echo "The crate then builds for the host and reaches no probe." >&2
          exit 1
        fi
      done
      touch $out
    '';

  # A push site builds its metric with the `metric!` macro, which checks
  # the name in a `const` block, so a bad name stops the build. A direct
  # call of the metric constructor, or of the name constructor, passes the
  # compiler with a name that was checked nowhere, or checked only when
  # the line runs. The compiler cannot tell the two apart, so this gate
  # reads the sources for the calls. `excludeDirs` names the directories
  # the scan skips: the crate that owns the constructors in the open tree,
  # and the open tree itself in the tree that consumes it, where this same
  # gate already ran.
  metricConstructor =
    {
      root,
      excludeDirs,
    }:
    let
      calls = [
        "Metric::new("
        "Name::new("
      ];
    in
    pkgs.runCommand "metric-constructor" { } ''
      cd ${root}
      hits=$(grep -rnF ${
        lib.concatMapStringsSep " " (call: "-e ${lib.escapeShellArg call}") calls
      } --include='*.rs' . \
        ${lib.concatMapStringsSep " " (dir: "--exclude-dir=${lib.escapeShellArg dir}") excludeDirs} \
        || true)
      if [ -n "$hits" ]; then
        echo "These lines call ${lib.concatStringsSep " or " calls} directly:" >&2
        echo "$hits" | sed 's/^/  /' >&2
        echo "" >&2
        echo "Build the metric with the metric! macro, and take a shared" >&2
        echo "name from the catalog. Both check the name of the series" >&2
        echo "when the crate is compiled." >&2
        exit 1
      fi
      touch $out
    '';
}
