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

# Shared dependency artifacts for the two checks cargo builds as a whole
# on crane: the esp32s3 checks for their `-Z build-std` Xtensa target,
# and the bmm cross check of host-runner for its musl target. Without
# reused artifacts each recompiles the full dependency graph.
{
  src,
  helpers,
  bmmCraneLib,
  espCraneLib,
  espCargoVendorDir,
  xtensaEspToolchain,
  bmmCc,
}:
let
  # A check-only bag: dependencies are `cargo check`ed, never linked.
  checkBag =
    lib: extra:
    lib.buildDepsOnly (
      {
        inherit src;
        doCheck = false;
        cargoBuildCommand = "true";
      }
      // extra
    );

in
rec {
  bmmArtifacts = checkBag bmmCraneLib (
    {
      cargoExtraArgs = "-p host-runner --target arm-unknown-linux-musleabi";
      # ubos-webui's build.rs runs as part of host-runner's dependency
      # closure, so it needs the hermetic dist too.
      nativeBuildInputs = [
        bmmCc
      ];
      CARGO_TARGET_ARM_UNKNOWN_LINUX_MUSLEABI_LINKER = "${bmmCc}/bin/armv5tel-unknown-linux-musleabi-gcc";
      ${helpers.ccVar "arm-unknown-linux-musleabi"} = "${bmmCc}/bin/armv5tel-unknown-linux-musleabi-gcc";
    }
    // helpers.webuiDistEnv
  );

  # The esp deps bag stays check-only. crane's buildDepsOnly stubs the
  # esp32s3 bin with a dummy `main`, and a real `cargo build` fails to
  # link that stub under `-Z build-std=core`: the esp crt0 references
  # `main` while the crate is `#![no_main]`. A `cargo check` never
  # links, so it builds. The esp release builds therefore keep
  # `cargoArtifacts = null` and rebuild their dependency closure; the
  # other targets' bags do codegen and the builds reuse them.
  espArtifacts = checkBag espCraneLib (
    {
      cargoVendorDir = espCargoVendorDir;
      cargoExtraArgs = builtins.concatStringsSep " " [
        "-p esp32s3@0.1.0"
        "-p esp32s3-tsens"
        "--target xtensa-esp32s3-none-elf"
        "-Z build-std=core"
      ];
      nativeBuildInputs = [ xtensaEspToolchain ];
    }
    // helpers.espCrossEnv
  );
}
