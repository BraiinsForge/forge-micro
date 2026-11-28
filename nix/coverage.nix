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

# The coverage checks of the host test rows: the line coverage of each
# row with the CRAP score of each function, and a floor of covered
# lines. A tree states its rows to `coverageSuite` and gets both, so a
# row cannot have one without the other.
{
  pkgs,
  src,
  hostRustToolchain,
  # See nix/units.nix.
  hostPlan,
  packageOf,
  testsOf,
}:
let
  inherit (pkgs) lib;

  # The CRAP report over each host crate's coverage; see nix/cargo-crap.nix.
  cargoCrap = import ./cargo-crap.nix { inherit pkgs; };

  # The test modules a crate keeps under `src/tests/`. The coverage
  # report leaves them out, so cargo-crap would score their functions as
  # if no test ran them.
  testModules = "**/tests/**";

  # The cargo-crap arguments that leave the test modules and `skipped`,
  # a list of globs, out of a score.
  crapExcludes =
    skipped:
    lib.escapeShellArgs (
      lib.concatMap (glob: [
        "--exclude"
        glob
      ]) ([ testModules ] ++ skipped)
    );

  # The line coverage of one host crate by its own tests, from the test
  # runs of its plan with the crate's own units instrumented: `lcov.info`
  # for tools that read it, an HTML report under `html/`, and
  # `summary.txt`. It counts the crate's own sources without its test,
  # example and bench files, as cargo-llvm-cov reports them. `crap.json`
  # scores each function of the crate from its complexity and that
  # coverage, as a report that no check reads. The scores leave out
  # the files that match `skipped`: a file the row does not
  # compile, which cargo-crap would score as untested, and a file of
  # task shells that only run against a real device. The line coverage
  # still counts every file the row compiles.
  #
  # A source has more than one name in the profiles: a test is compiled
  # in a copy of the source, and a library unit it links with its paths
  # remapped to the package's name. Every name ends in the package
  # directory and the file below it, so lcov cuts each name to that and
  # merges the records of one file into one. The report counts lines
  # alone, because each compile and each instantiation of a function has
  # a record of its own, which would count one function many times.
  coverageReport =
    skipped: name: row:
    let
      # Only the crate's own units are instrumented, so every unit it
      # depends on is the one its host plan builds.
      plan = hostPlan (
        row
        // {
          overrides.${row.crate}.rustflags = [
            "-C"
            "instrument-coverage"
          ];
        }
      );
      package = packageOf plan row.crate;
      tests = testsOf plan row.crate;
      tools = "${hostRustToolchain}/lib/rustlib/${pkgs.stdenv.hostPlatform.rust.rustcTarget}/bin";
      dir = package.manifestDir;
    in
    pkgs.runCommand name
      {
        nativeBuildInputs = [
          pkgs.lcov
          pkgs.jq
          cargoCrap
        ];
      }
      ''
        shopt -s nullglob
        profiles=() objects=()
        for test in ${lib.escapeShellArgs tests}; do
          profiles+=("$test"/profraw/*.profraw)
          for object in "$test"/objects/*; do objects+=(-object "$object"); done
        done
        if [ ''${#profiles[@]} -eq 0 ]; then
          echo "${name}: The tests of ${row.crate} wrote no coverage profile." >&2
          exit 1
        fi
        ${tools}/llvm-profdata merge -sparse "''${profiles[@]}" -o merged.profdata
        # llvm-cov takes its first executable without `-object`.
        ${tools}/llvm-cov export -format=lcov -instr-profile merged.profdata "''${objects[@]:1}" |
          sed 's|^SF:.*/\(${dir}/\)|SF:\1|' > names.info
        # Naming an error class twice also silences its warning.
        lcovFlags=(--quiet --rc function_coverage=0 --ignore-errors unused,unused)
        mkdir -p "$out"
        lcov "''${lcovFlags[@]}" --add-tracefile names.info --output-file merged.info
        lcov "''${lcovFlags[@]}" --extract merged.info '${dir}/*' --output-file own.info
        lcov "''${lcovFlags[@]}" --remove own.info '*/tests/*' '*/examples/*' '*/benches/*' \
          --output-file "$out/lcov.info"
        # genhtml reads each source by the name lcov gives it.
        cd ${src}
        genhtml "''${lcovFlags[@]}" --output-directory "$out/html" "$out/lcov.info"
        lcov --rc function_coverage=0 --summary "$out/lcov.info" 2>&1 | tee "$out/summary.txt"
        # One function to a line, without the uncovered ranges the HTML
        # report shows, so grep finds a function with its score.
        cargo-crap --lcov "$out/lcov.info" --path ${dir} ${crapExcludes skipped} --no-cache --format json --sort file |
          jq -r '"{\"version\": \(.version | tojson), \"entries\": [",
            (.entries | map(del(.uncovered) | tojson) | join(",\n")), "]}"' > "$out/crap.json"
      '';

  # The line-coverage floors of a tree: one check that fails when a row
  # of `reports`, the `coverageReport` of each row by name, covers a
  # smaller share of its lines than `floors` asks of it in percent, or
  # has no lines to cover while its floor is above zero. It prints every
  # row, so one run names each row that fell. The two sets name the same
  # rows, or the evaluation stops, so a new row cannot go without a
  # floor and a floor cannot outlive its row.
  coverageFloors =
    reports: floors:
    let
      rows = lib.attrNames reports;
      floored = lib.attrNames floors;
    in
    assert lib.assertMsg (rows == floored) ''
      The coverage rows and the coverage floors do not match.
      Rows without a floor: ${toString (lib.subtractLists floored rows)}
      Floors without a row: ${toString (lib.subtractLists rows floored)}
    '';
    pkgs.runCommand "line-coverage-floors" { } ''
      fail=0
      check() {
        local row=$1 lcov=$2 floor=$3 hit found
        read -r hit found < <(awk -F: '/^LH:/ { hit += $2 } /^LF:/ { found += $2 } END { print hit + 0, found + 0 }' "$lcov")
        printf '%-24s %6d of %6d lines, floor %3d%%\n' "$row" "$hit" "$found" "$floor"
        if [ "$found" -eq 0 ] && [ "$floor" -gt 0 ]; then
          echo "ERROR: The report of $row has no lines. Check that its tests ran." >&2
          fail=1
        elif [ $((hit * 100)) -lt $((floor * found)) ]; then
          echo "ERROR: The tests of $row cover less than $floor% of its lines. Add tests for the new code." >&2
          fail=1
        fi
      }
      ${lib.concatStrings (
        lib.mapAttrsToList (row: report: ''
          check ${row} ${report}/lcov.info ${toString floors.${row}}
        '') reports
      )}
      [ "$fail" -eq 0 ]
      touch $out
    '';
in
{
  # The checks of a tree's host test rows: `coverage-<row>` and
  # `line-coverage-floors`.
  #
  # - `rows` are the `hostPlan` rows by name, and `fee` is the default
  #   fee switch of each.
  # - `floors` are the percentages for `coverageFloors`, by row.
  # - `skipped` lists the globs a row leaves out of its CRAP scores.
  #
  # A row without a floor, a floor without a row and a skip list without
  # a row each stop the evaluation, so none can outlive or go without
  # the other.
  coverageSuite =
    {
      rows,
      floors,
      fee ? { },
      skipped ? { },
    }:
    let
      reports = lib.mapAttrs (
        row: spec: coverageReport (skipped.${row} or [ ]) "coverage-${row}" ({ inherit fee; } // spec)
      ) rows;
      unknownSkips = lib.subtractLists (lib.attrNames rows) (lib.attrNames skipped);
    in
    assert lib.assertMsg (
      unknownSkips == [ ]
    ) "A skip list has no coverage row: ${toString unknownSkips}.";
    lib.mapAttrs' (row: report: lib.nameValuePair "coverage-${row}" report) reports
    // {
      line-coverage-floors = coverageFloors reports floors;
    };
}
