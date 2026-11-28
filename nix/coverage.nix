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

# The line coverage of the host test rows. A tree states its rows to
# `coverageSuite` and gets a `coverage-<row>` check for each.
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

  # The line coverage of one host crate by its own tests, from the test
  # runs of its plan with the crate's own units instrumented: `lcov.info`
  # for tools that read it, an HTML report under `html/`, and
  # `summary.txt`. It counts the crate's own sources without its test,
  # example and bench files, as cargo-llvm-cov reports them.
  #
  # A source has more than one name in the profiles: a test is compiled
  # in a copy of the source, and a library unit it links with its paths
  # remapped to the package's name. Every name ends in the package
  # directory and the file below it, so lcov cuts each name to that and
  # merges the records of one file into one. The report counts lines
  # alone, because each compile and each instantiation of a function has
  # a record of its own, which would count one function many times.
  coverageReport =
    name: row:
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
    pkgs.runCommand name { nativeBuildInputs = [ pkgs.lcov ]; } ''
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
    '';
in
{
  # The `coverage-<row>` check of each of `rows`, the `hostPlan` rows by
  # name, with `fee` as the default fee switch of each.
  coverageSuite =
    {
      rows,
      fee ? { },
    }:
    lib.mapAttrs' (
      row: spec:
      lib.nameValuePair "coverage-${row}" (coverageReport "coverage-${row}" ({ inherit fee; } // spec))
    ) rows;
}
