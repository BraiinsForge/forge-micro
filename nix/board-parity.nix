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

  # An awk program over one board's sources. It prints `file:line name`
  # for every task the crate declares itself.
  #
  # A comment line is skipped before anything else, because ten boards
  # quote the attribute inside the doc comment on `stratum_client_task`
  # and the gate would fail on all ten.
  #
  # After the attribute, further attributes and blank lines are allowed
  # before the function: braiins-axe writes its `boot` attribute above a
  # comment block and an `#[allow]`. Any other line ends the search, so
  # an attribute that names no task cannot attach itself to a function
  # further down the file.
  scanProgram = ''
    /^[[:space:]]*\/\// { next }
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
  # board list is every directory under its `firmware/` less `exclude`,
  # rather than a list written out here: an explicit list is what lets
  # the next board arrive ungated, which is the failure this gate
  # exists to stop. `holders` maps a board name to the tasks that are
  # genuinely its own.
  boardSkeletonParity =
    {
      root,
      exclude ? [ ],
      holders ? { },
    }:
    let
      firmwareDir = root + "/firmware";
      entries = builtins.readDir firmwareDir;
      boards = lib.naturalSort (
        lib.filter (name: entries.${name} == "directory" && !(builtins.elem name exclude)) (
          builtins.attrNames entries
        )
      );
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
            if ! grep -rqF -- "$call" "$board/src"; then
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
}
