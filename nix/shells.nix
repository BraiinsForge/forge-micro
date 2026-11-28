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

# Interactive development shells, one per compilation universe in the
# tree. Shared knobs — the webui build env, the cross-CC settings —
# arrive as arguments so every shell stays consistent with the flake
# checks.
#
# Every shell here sets UBOS_WEBUI_DIST to the hermetic dashboard build
# the checks read, so a `cargo build` in a shell embeds the same pages a
# check does and never runs `npm`. The one path the checks do not
# exercise is the one that broke: a half-finished `npm ci` left a
# `node_modules` that build.rs took for an install, and every shell
# build failed while every gate stayed green. The variable is applied
# to the whole set this file returns rather than per shell, so a shell
# added to that set cannot miss it. That guarantee stops at this file:
# a consuming repo that hosts shells of its own wraps them itself, and
# `helpers.withWebuiDist` is exported for that. The dist
# depends on the frontend sources alone, so its store path moves only
# when they do and a dirty Rust tree rebuilds nothing on its account.
#
# These shells flash over a probe attached to the machine they run on.
# `probe-rs-runner` takes the probe from `--probe`, or from
# `$PROBE_SERIAL` when several are plugged in, and auto-selects
# otherwise.
#
# `crateRoot` prefixes the greetings' `cd` lines for a shell entered
# outside this tree.
{
  pkgs,
  toolchains,
  esp,
  helpers,
  crateRoot ? "",
}:
let
  # The linkPatchedCrates hook materializes the pinned-and-patched
  # upstream crates at patches/<crate> on shell entry; see nix/lib.nix.
  inherit (helpers) linkPatchedCrates;
  inherit (toolchains)
    rustToolchain
    hostRustToolchain
    bmmRustToolchain
    bmmCc
    ;
  inherit (esp) espToolchain xtensaEspToolchain;

  # Cargo runner shared by the probe-rs boards (bmm-pico, rp2040,
  # rp2350), packaged as a writeShellApplication so it self-lints and
  # carries its probe-rs-runner PATH dep; each crate's .cargo/config.toml
  # sets PROBE_RS_CHIP and points its runner at this binary.
  probeRsBoardRunner = pkgs.callPackage ../pkgs/probe-rs-board-runner { };

  # The remaining script packages, the same self-linting
  # writeShellApplication/writePython3Bin wrappers with their PATH deps
  # baked in: the two bootloader cargo runners, the host-runner TAP
  # setup, and the host-runner cross-build cargo runner.
  rp2040BootloaderRunner = pkgs.callPackage ../pkgs/rp2040-bootloader-runner { };
  rp2350BootloaderRunner = pkgs.callPackage ../pkgs/rp2350-bootloader-runner { };
  setupNetwork = pkgs.callPackage ../pkgs/setup-network { };
  hostRunnerRun = pkgs.callPackage ../pkgs/host-runner-run { };
in
# The hermetic dashboard build, on every shell; see the header.
pkgs.lib.mapAttrs (_: helpers.withWebuiDist) {
  arm = pkgs.mkShell (
    {
      buildInputs = with pkgs; [
        rustToolchain
        cargo-binutils
        cargo-bloat
        probe-rs-tools
        probe-rs-runner
        pkg-config
        libudev-zero
        picocom
        usbutils
        # Cross C compiler for crates with C sources, for example
        # secp256k1-sys via the unconditional ii-sv2 dep.
        # Each rustc target needs a `CC_<target>` env var, merged in
        # below via helpers.armCc; cc-rs' fallback to host `gcc` does
        # not work for thumb targets.
        gcc-arm-embedded
        # ubos-webui's Rsbuild/Preact dev loop (`npm run dev`). Not for
        # build.rs: every shell hands it the hermetic dist, see the
        # header.
        nodejs_26
        # Byte-exact A/B size measurement of the ubos-webui pages; the
        # crate's build.rs embeds Zopfli-gzipped bytes, so the CLI must
        # match the encoder to make the crate CLAUDE.md numbers honest.
        zopfli
        # Unused-dependency lint, matching the cargo-machete check.
        cargo-machete
        # Licence, duplicate-version and source lint, matching the
        # cargo-deny check.
        cargo-deny
        # Builds combined BOOTSEL UF2 images from app and bootloader.
        picotool
        # Automatic fixup commits for the documented fixup workflow.
        git-absorb
        # The cargo runner of each bootloader crate. Each names its own
        # chip, so there is one per crate.
        rp2040BootloaderRunner
        rp2350BootloaderRunner
        # The shared cargo runner the other ARM boards' .cargo/config.toml
        # point their `runner` at.
        probeRsBoardRunner
      ];

      RUST_ANALYZER_SERVER_PATH = "${rustToolchain}/bin/rust-analyzer";

      shellHook = ''
        ${linkPatchedCrates}
        echo "firmware workspace dev environment ready (ARM boards)"
        echo ""
        echo "  cd ${crateRoot}firmware/hashnode  && cargo build --release  # Hashnode RP2350B BM1370"
        echo "  cd ${crateRoot}firmware/forge-axe && cargo build --release  # Forge Axe RP2350B W5500 BM1370"
        echo "  cd ${crateRoot}firmware/bmm-pico  && cargo build --release  # BMM Adapter RP2350 W5500"
        echo "  cd ${crateRoot}firmware/rp2040    && cargo build --release  # RP2040 W6100"
        echo "  cd ${crateRoot}firmware/rp2350    && cargo build --release  # RP2350 W6100"
        echo ""
        echo "Inspect binary size: cargo size --release -- -A"
        echo ""
        echo "For ESP32-S3 board: nix develop .#esp"
      '';
    }
    // helpers.armCc "thumbv6m-none-eabi"
    // helpers.armCc "thumbv8m.main-none-eabihf"
  );

  # ESP32-S3, Xtensa. The shell builds with the same pinned esp-rs
  # rustc and espressif cross-gcc the flake checks use, from nix/esp.nix,
  # so a shell build and a check build agree on every byte and a fresh
  # checkout downloads nothing on entry. The Xtensa rustc must shadow
  # every other rustc on PATH, so no second toolchain is listed here.
  esp = pkgs.mkShell (
    {
      buildInputs = [
        pkgs.probe-rs-tools
        pkgs.probe-rs-runner
      ]
      ++ (with pkgs; [
        espToolchain
        xtensaEspToolchain
        cargo-binutils
        cargo-bloat
        espflash
        pkg-config
        libudev-zero
        # Automatic fixup commits for the documented fixup workflow.
        git-absorb
      ]);

      # The esp-rs release ships no rust-analyzer; the host toolchain's
      # is the same rust version and reads its sysroot from the Xtensa
      # rustc on PATH.
      RUST_ANALYZER_SERVER_PATH = "${hostRustToolchain}/bin/rust-analyzer";
      LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";

      shellHook = ''
        ${linkPatchedCrates}
        echo "firmware workspace dev environment ready (ESP32-S3)"
        echo ""
        echo "  cd ${crateRoot}firmware/esp32s3 && cargo build --release  # Waveshare ESP32-S3-LCD-Driver-Board W5100S"
        echo ""
        echo "Inspect binary size: cargo size --release -- -A"
        echo ""
        echo "For ARM boards: nix develop .#arm"
      '';
    }
    // helpers.espCrossEnv
  );

  host = pkgs.mkShell {
    buildInputs = [
      hostRustToolchain
      pkgs.cargo-llvm-cov
      # Unused-dependency lint, matching the cargo-machete check.
      pkgs.cargo-machete
      # Licence, duplicate-version and source lint, matching the
      # cargo-deny check.
      pkgs.cargo-deny
      # Brings up tap0 for the native host-runner path.
      setupNetwork
    ];

    RUST_ANALYZER_SERVER_PATH = "${hostRustToolchain}/bin/rust-analyzer";

    shellHook = ''
      ${linkPatchedCrates}
      echo "firmware workspace dev environment ready (host-runner, native, TAP)"
      echo ""
      echo "  cargo run -p host-runner"
      echo ""
      echo "For miniminer cross-build: nix develop .#bmm"
    '';
  };

  bmm = pkgs.mkShell {
    buildInputs = [
      bmmRustToolchain
      bmmCc
      # The host-runner cross-build cargo runner (bakes its own python);
      # it ssh/scp/sshpass's the binary to the target and streams logs.
      hostRunnerRun
      pkgs.openssh
      pkgs.sshpass
    ];

    CARGO_BUILD_TARGET = "arm-unknown-linux-musleabi";
    CARGO_TARGET_ARM_UNKNOWN_LINUX_MUSLEABI_LINKER = "${bmmCc}/bin/armv5tel-unknown-linux-musleabi-gcc";
    # cargo runs this on `cargo run`; a fixed store path, no longer a
    # working-tree script, so it needs no $PWD resolution.
    CARGO_TARGET_ARM_UNKNOWN_LINUX_MUSLEABI_RUNNER = pkgs.lib.getExe hostRunnerRun;
    RUST_ANALYZER_SERVER_PATH = "${bmmRustToolchain}/bin/rust-analyzer";

    shellHook = ''
      ${linkPatchedCrates}
      echo "firmware workspace dev environment ready (host-runner, BMM/miniminer cross)"
      echo ""
      echo "  cd ${crateRoot}host/host-runner && cargo build --release"
      echo "                         ${pkgs.lib.strings.replicate (builtins.stringLength crateRoot) " "}cargo run --release -- --hostname <miner-ip>"
      echo ""
      echo "See host-runner-run --help for more flags."
      echo ""
      echo "For native host-runner: nix develop .#host"
    '';
  };

  default = pkgs.mkShell {
    buildInputs = with pkgs; [
      picocom
      usbutils
    ];

    shellHook = ''
      echo "Available development shells:"
      echo ""
      echo "  nix develop .#arm   — ARM boards (RP2040, RP2350)"
      echo "  nix develop .#esp   — ESP32-S3 board"
      echo "  nix develop .#host  — host-runner, native"
      echo "  nix develop .#bmm   — host-runner, BMM/miniminer cross"
      echo ""
      echo "This default shell provides picocom and usbutils."
    '';
  };
}
