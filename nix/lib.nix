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

# Small helpers shared by more than one flake module: the
# `CC_<target>` env-var name cc-rs expects, the ARM cross-CC attrset
# every ARM crane derivation carries, the xtensa cross-build env every
# esp32s3 crane derivation carries, and the ubos-webui build-env attrs
# that every derivation and dev shell touching the dashboard must set
# so build.rs stamps a consistent rev and time. Defined once here so
# the copies in checks.nix, images.nix, and shells.nix cannot drift
# apart.
{
  pkgs,
  webuiBuildEnv,
  webuiDist,
  espToolchain,
  xtensaEspToolchain,
  patchedCrates,
  # Directory the workspace `[patch]` table points at, relative to the
  # repository root the shell is entered from. A consumer that imports
  # this module and reaches these crates through a submodule has to say
  # so: its `[patch]` paths are prefixed, and a hook writing to the
  # unprefixed path would create a directory cargo never reads.
  patchesDir ? "patches",
}:
rec {
  ccVar = target: "CC_${builtins.replaceStrings [ "-" "." ] [ "_" "_" ] target}";

  # Shell-hook fragment for every cargo-running dev shell. The
  # workspace `[patch]` table points at ${patchesDir}/<crate>, which the
  # repo no longer carries for the pinned-and-patched upstream crates;
  # symlink each materialized tree from the store into the working
  # tree so cargo resolves it. A real directory at the path is left
  # alone: it is either the not-yet-removed vendored tree on an older
  # commit or a local unpacked copy being hacked on, and both should
  # win over the store tree. The shells are entered from the
  # repository root by convention; elsewhere there is nothing to link.
  linkPatchedCrates = pkgs.lib.concatStrings (
    pkgs.lib.mapAttrsToList (name: drv: ''
      if [ -d "${patchesDir}" ] && { [ ! -e "${patchesDir}/${name}" ] || [ -L "${patchesDir}/${name}" ]; }; then
        ln -sfnT ${drv} "${patchesDir}/${name}"
      fi
    '') patchedCrates
  );

  armCc = target: {
    "${ccVar target}" = "${pkgs.gcc-arm-embedded}/bin/arm-none-eabi-gcc";
  };

  webuiEnvAttrs = {
    inherit (webuiBuildEnv) UBOS_WEBUI_BUILD_REV UBOS_WEBUI_BUILD_TIME;
  };

  # Points ubos-webui's build.rs at the hermetic Rsbuild output so the
  # crate check never runs `npm` in the offline sandbox. build.rs reads
  # the two prebuilt pages from this directory and Brotli-compresses
  # them.
  webuiDistEnv = {
    UBOS_WEBUI_DIST = "${webuiDist}";
  };

  # Xtensa cross-build env every esp32s3 crane derivation carries:
  # rust-src for -Z build-std, the espressif cross-gcc for the
  # secp256k1-sys C build script, and -mlongcalls so its memset/memcpy
  # can be reached from text sections beyond the xtensa call8 range.
  espCrossEnv = {
    RUST_SRC_PATH = "${espToolchain}/lib/rustlib/src/rust/library";
    CC_xtensa_esp32s3_none_elf = "${xtensaEspToolchain}/bin/xtensa-esp32s3-elf-gcc";
    CFLAGS_xtensa_esp32s3_none_elf = "-mlongcalls";
  };

  # Linker rustflags for the esp32s3 target. Crane runs cargo from the
  # workspace root, so firmware/esp32s3/.cargo/config.toml is out of
  # cargo's discovery path; without -Tlinkall.x the linker uses default
  # section placement and secp256k1-sys's memset/memcpy calls overflow
  # the xtensa windowed-call range.
  espRustflags = {
    CARGO_TARGET_XTENSA_ESP32S3_NONE_ELF_RUSTFLAGS = "-Clink-arg=-Tlinkall.x -Clink-arg=-Tdefmt.x -Clink-arg=-nostartfiles";
  };
}
