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

# Interactive development shells, one per compilation universe that
# ships on the public mirror. The internal-only shells — esp,
# hardware, logic, riscv — live in nix/private-shells.nix, which the
# publish strip removes with the nix/private.nix wrapper that merges
# them in. Shared knobs — the bench probe-rs endpoint
# and the webui build env — arrive as arguments so every shell stays
# consistent with the flake checks. `bench` is null on the public
# mirror, which ships no nuc/; the bench-only pieces degrade away.
{
  pkgs,
  toolchains,
  deployRsPackage,
  bench,
  helpers,
}:
let
  # The linkPatchedCrates hook materializes the pinned-and-patched
  # upstream crates at patches/<crate> on shell entry; see nix/lib.nix.
  inherit (helpers) webuiEnvAttrs linkPatchedCrates;
  inherit (toolchains)
    rustToolchain
    hostRustToolchain
    bmmRustToolchain
    bmmCc
    ;

  # Bench probe-rs endpoint env, so interactive flashing defaults to
  # the NUC's probe-rs-server. Empty on the public mirror, where the
  # runner falls back to the locally attached probe.
  benchEnv = pkgs.lib.optionalAttrs (bench != null) {
    PROBE_RS_REMOTE_HOST = bench.probeRsHost;
    PROBE_RS_REMOTE_TOKEN = bench.probeRsToken;
  };

  # Cargo runner shared by the private probe-rs boards (bmm-pico, rp2040,
  # rp2350), packaged as a writeShellApplication so it self-lints and
  # carries its probe-rs-runner PATH dep; each crate's .cargo/config.toml
  # sets PROBE_RS_CHIP and points its runner at this binary. Bench-gated,
  # so mirror evaluation never forces the stripped package path.
  probeRsBoardRunner = pkgs.callPackage ../pkgs/probe-rs-board-runner { };

  # Public script packages, the same writeShellApplication/writePython3Bin
  # wrappers (self-linting, PATH deps baked). Kept on the mirror, so wired
  # unconditionally into their shells: the rp2350-bootloader cargo runner,
  # the HTTP load tester, the host-runner TAP setup, and the host-runner
  # cross-build cargo runner.
  rp2350BootloaderRunner = pkgs.callPackage ../pkgs/rp2350-bootloader-runner { };
  httpLoadTest = pkgs.callPackage ../pkgs/http-load-test { };
  setupNetwork = pkgs.callPackage ../pkgs/setup-network { };
  hostRunnerRun = pkgs.callPackage ../pkgs/host-runner-run { };
in
{
  arm = pkgs.mkShell (
    {
      buildInputs =
        with pkgs;
        [
          rustToolchain
          cargo-binutils
          cargo-bloat
          probe-rs-tools-remote
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
          # Used by ubos-webui's build.rs to minify the dashboard
          # CSS/JS before gzipping into OUT_DIR.
          esbuild
          # Unused-dependency lint, matching the cargo-machete check.
          cargo-machete
          # Builds combined BOOTSEL UF2 images from app and bootloader.
          picotool
          # Automatic fixup commits for the documented fixup workflow.
          git-absorb
          # rp2350-bootloader's cargo runner; public, so ships on the mirror.
          rp2350BootloaderRunner
        ]
        ++ pkgs.lib.optionals (bench != null) [
          deployRsPackage
          probeRsBoardRunner
        ];

      RUST_ANALYZER_SERVER_PATH = "${rustToolchain}/bin/rust-analyzer";

      inherit (webuiEnvAttrs) UBOS_WEBUI_BUILD_REV UBOS_WEBUI_BUILD_TIME;

      shellHook = ''
        ${linkPatchedCrates}
        echo "ubos-main dev environment ready (ARM boards)"
        echo ""
        echo "  cd firmware/hashnode  && cargo build --release  # Hashnode RP2350B BM1370"
      ''
      # The other boards are private and their crates ship only in the
      # internal tree, so the mirror banner names hashnode alone.
      + pkgs.lib.optionalString (bench != null) ''
        echo "  cd firmware/bmm-pico  && cargo build --release  # BMM Adapter RP2350 W5500"
        echo "  cd firmware/rp2040    && cargo build --release  # RP2040 W5500"
        echo "  cd firmware/rp2350    && cargo build --release  # RP2350 W6100"
        echo "  cd firmware/stm32f407 && cargo build --release  # STM32F407 LAN8720"
        echo "  cd firmware/stm32f439 && cargo build --release  # STM32F439 LAN8742A"
      ''
      + ''
        echo ""
        echo "Inspect binary size: cargo size --release -- -A"
        echo ""
      ''
      + pkgs.lib.optionalString (bench != null) ''
        echo "For ESP32-S3 board: nix develop .#esp"
        echo "Deploy to NUC:      deploy"
      '';
    }
    // helpers.armCc "thumbv6m-none-eabi"
    // helpers.armCc "thumbv7em-none-eabihf"
    // helpers.armCc "thumbv8m.main-none-eabihf"
    // benchEnv
  );

  host = pkgs.mkShell {
    buildInputs = [
      hostRustToolchain
      pkgs.cargo-llvm-cov
      # Used by ubos-webui's build.rs to minify the dashboard
      # CSS/JS before gzipping into OUT_DIR.
      pkgs.esbuild
      # Unused-dependency lint, matching the cargo-machete check.
      pkgs.cargo-machete
      # Brings up tap0 for the native host-runner path.
      setupNetwork
    ];

    RUST_ANALYZER_SERVER_PATH = "${hostRustToolchain}/bin/rust-analyzer";

    inherit (webuiEnvAttrs) UBOS_WEBUI_BUILD_REV UBOS_WEBUI_BUILD_TIME;

    shellHook = ''
      ${linkPatchedCrates}
      echo "ubos-main dev environment ready (host-runner, native, TAP)"
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
      # Used by ubos-webui's build.rs to minify the dashboard
      # CSS/JS before gzipping into OUT_DIR.
      pkgs.esbuild
    ];

    CARGO_BUILD_TARGET = "arm-unknown-linux-musleabi";
    CARGO_TARGET_ARM_UNKNOWN_LINUX_MUSLEABI_LINKER = "${bmmCc}/bin/armv5tel-unknown-linux-musleabi-gcc";
    # cargo runs this on `cargo run`; a fixed store path, no longer a
    # working-tree script, so it needs no $PWD resolution.
    CARGO_TARGET_ARM_UNKNOWN_LINUX_MUSLEABI_RUNNER = pkgs.lib.getExe hostRunnerRun;
    RUST_ANALYZER_SERVER_PATH = "${bmmRustToolchain}/bin/rust-analyzer";

    inherit (webuiEnvAttrs) UBOS_WEBUI_BUILD_REV UBOS_WEBUI_BUILD_TIME;

    shellHook = ''
      ${linkPatchedCrates}
      echo "ubos-main dev environment ready (host-runner, BMM/miniminer cross)"
      echo ""
      echo "  cd host/host-runner && cargo build --release"
      echo "                         cargo run --release -- --hostname redacted50.1"
      echo ""
      echo "See host-runner-run --help for more flags."
      echo ""
      echo "For native host-runner: nix develop .#host"
    '';
  };

  default = pkgs.mkShell {
    buildInputs =
      with pkgs;
      [
        picocom
        usbutils
        # Drives concurrent HTTP load at a board's dashboard API.
        httpLoadTest
      ]
      ++ pkgs.lib.optionals (bench != null) [ deployRsPackage ];

    shellHook = ''
      echo "ubos-main — available development shells:"
      echo ""
      echo "  nix develop .#arm   — ARM boards (RP2040, RP2350, STM32F407, STM32F439)"
    ''
    + pkgs.lib.optionalString (bench != null) ''
      echo "  nix develop .#esp   — ESP32-S3 board"
    ''
    + ''
      echo "  nix develop .#host  — host-runner, native"
      echo "  nix develop .#bmm   — host-runner, BMM/miniminer cross"
      echo ""
      echo "This default shell provides picocom, usbutils, and http-load-test."
    ''
    + pkgs.lib.optionalString (bench != null) ''
      echo "deploy-rs is included for NUC deployment."
    '';
  };
}
