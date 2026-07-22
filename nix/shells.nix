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

# Interactive development shells, one per compilation universe plus
# the hardware/logic tooling. Shared knobs — the bench probe-rs
# endpoint and the webui build env — arrive as arguments so every shell
# stays consistent with the flake checks. `bench` is null on the public
# mirror, which ships no nuc/; the bench-only pieces degrade away.
{
  pkgs,
  toolchains,
  esp,
  redacted,
  deployRsPackage,
  bench,
  helpers,
}:
let
  inherit (helpers) webuiEnvAttrs;
  inherit (toolchains)
    rustToolchain
    espRustToolchain
    hostRustToolchain
    bmmRustToolchain
    bmmCc
    ;
  inherit (esp) espToolchainVersion patchEspBinaries;
  inherit (redacted) redactedPkgs redactedCapturePython redactedRun;

  # Bench probe-rs endpoint env, so interactive flashing defaults to
  # the NUC's probe-rs-server. Empty on the public mirror, where the
  # runner falls back to the locally attached probe.
  benchEnv = pkgs.lib.optionalAttrs (bench != null) {
    PROBE_RS_REMOTE_HOST = bench.probeRsHost;
    PROBE_RS_REMOTE_TOKEN = bench.probeRsToken;
  };
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
        ]
        ++ pkgs.lib.optionals (bench != null) [ deployRsPackage ];

      RUST_ANALYZER_SERVER_PATH = "${rustToolchain}/bin/rust-analyzer";

      inherit (webuiEnvAttrs) UBOS_WEBUI_BUILD_REV UBOS_WEBUI_BUILD_TIME;

      shellHook = ''
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
        echo "For ESP32-S3 board: nix develop .#esp"
      ''
      + pkgs.lib.optionalString (bench != null) ''
        echo "Deploy to NUC:      deploy"
      '';
    }
    // helpers.armCc "thumbv6m-none-eabi"
    // helpers.armCc "thumbv7em-none-eabihf"
    // helpers.armCc "thumbv8m.main-none-eabihf"
    // benchEnv
  );

  esp = pkgs.mkShell (
    {
      buildInputs = [
        pkgs.probe-rs-tools-remote
        pkgs.probe-rs-runner
      ]
      ++ (with pkgs; [
        espRustToolchain
        cargo-binutils
        cargo-bloat
        espup
        espflash
        patchelf
        patchEspBinaries
        pkg-config
        libudev-zero
        perl
        stdenv.cc.cc.lib
        glibc
        zlib
        libxml2
        openssl
        libffi
        # Used by ubos-webui's build.rs to minify the dashboard
        # CSS/JS before gzipping into OUT_DIR.
        esbuild
        # Automatic fixup commits for the documented fixup workflow.
        git-absorb
      ]);

      NIX_LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath [
        pkgs.stdenv.cc.cc.lib
        pkgs.glibc
        pkgs.zlib
        pkgs.libxml2
        pkgs.openssl
        pkgs.libffi
      ];
      NIX_LD = "${pkgs.glibc}/lib/ld-linux-x86-64.so.2";

      RUST_ANALYZER_SERVER_PATH = "${espRustToolchain}/bin/rust-analyzer";
      LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";

      inherit (webuiEnvAttrs) UBOS_WEBUI_BUILD_REV UBOS_WEBUI_BUILD_TIME;

      shellHook = ''
        export RUSTUP_HOME="$PWD/.rustup"
        export CARGO_HOME="$PWD/.cargo"
        mkdir -p "$RUSTUP_HOME" "$CARGO_HOME"

        if [ ! -d "$RUSTUP_HOME/toolchains/esp/bin" ]; then
          # Remove partial state from a previous failed install.
          # espup aborts with "File exists" on pre-created LLVM
          # symlinks when the toolchain dir is present but incomplete.
          rm -rf "$RUSTUP_HOME/toolchains/esp"
          # espup also writes a global $HOME/.espup/esp-clang symlink
          # pointing at the last-installed toolchain. A stale link
          # left by another worktree makes espup abort with EEXIST
          # even though no local state remains.
          rm -f "$HOME/.espup/esp-clang"
          echo "Installing ESP32-S3 Xtensa toolchain..."
          # Version pinned for reproducibility. The same pin feeds
          # the Xtensa flake-check derivation in nix/esp.nix.
          espup install --targets esp32s3 --toolchain-version ${espToolchainVersion} || true
          patch-esp-binaries
        fi

        # Find GCC and clang paths dynamically to avoid hardcoding version strings.
        XTENSA_GCC_PATH="$(echo "$RUSTUP_HOME"/toolchains/esp/xtensa-esp-elf/esp-*/xtensa-esp-elf/bin | head -1)"
        LIBCLANG_TOOLCHAIN_PATH="$(echo "$RUSTUP_HOME"/toolchains/esp/xtensa-esp32-elf-clang/esp-*/esp-clang/lib | head -1)"

        export PATH="$XTENSA_GCC_PATH:$RUSTUP_HOME/toolchains/esp/bin:$CARGO_HOME/bin:$PATH"

        # `cc-rs`, via secp256k1-sys, an unconditional ii-sv2 dep, needs
        # an explicit cross-compiler. `-mlongcalls` keeps memset /
        # memcpy reachable past the xtensa `call8` range.
        export CC_xtensa_esp32s3_none_elf="$XTENSA_GCC_PATH/xtensa-esp32s3-elf-gcc"
        export CFLAGS_xtensa_esp32s3_none_elf="-mlongcalls"

        if [ -d "$LIBCLANG_TOOLCHAIN_PATH" ]; then
          export LIBCLANG_PATH="$LIBCLANG_TOOLCHAIN_PATH"
        fi

        echo "ubos-main dev environment ready (ESP32-S3)"
        echo ""
        echo "  cd firmware/esp32s3 && cargo build --release  # Waveshare ESP32-S3-LCD-Driver-Board W5100S"
        echo ""
        echo "Inspect binary size: cargo size --release -- -A"
        echo ""
        echo "For ARM boards: nix develop .#arm"
      '';
    }
    // benchEnv
  );

  hardware = pkgs.mkShell {
    packages = [ pkgs.kicad-small ];
  };

  # redacted Logic 2 headless trace capture. `redacted-run` wraps the
  # Logic lifecycle; the bundled python has the automation client.
  logic = pkgs.mkShell {
    buildInputs = [
      redactedRun
      redactedCapturePython
      redactedPkgs.redacted-logic-2
    ];
    shellHook = ''
      echo "ubos-main redacted Logic 2 headless capture environment"
      echo ""
      echo "  redacted-run python tools/redacted_capture_uart.py \\"
      echo "      --seconds 20 --baud 115200 --out /tmp/redacted-cap"
      echo ""
      echo "  redacted-run python tools/redacted_capture_i2c.py \\"
      echo "      --seconds 12 --reset-tty /dev/ttyUSB0 --out /tmp/i2c-cap"
      echo ""
      echo "redacted-run starts Logic 2 headless with the automation"
      echo "server enabled, waits for the gRPC port, runs the given"
      echo "command, then tears Logic down. Decoded frames land in"
      echo "<out>/decoded.csv."
      echo ""
      echo "Install assets/60-redacted.rules first (see docs/SETUP.md)"
      echo "or the device will be invisible to Logic."
    '';
  };

  host = pkgs.mkShell {
    buildInputs = [
      hostRustToolchain
      pkgs.cargo-llvm-cov
      # Used by ubos-webui's build.rs to minify the dashboard
      # CSS/JS before gzipping into OUT_DIR.
      pkgs.esbuild
      # Unused-dependency lint, matching the cargo-machete check.
      pkgs.cargo-machete
    ];

    RUST_ANALYZER_SERVER_PATH = "${hostRustToolchain}/bin/rust-analyzer";

    inherit (webuiEnvAttrs) UBOS_WEBUI_BUILD_REV UBOS_WEBUI_BUILD_TIME;

    shellHook = ''
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
      pkgs.python3
      pkgs.openssh
      pkgs.sshpass
      # Used by ubos-webui's build.rs to minify the dashboard
      # CSS/JS before gzipping into OUT_DIR.
      pkgs.esbuild
    ];

    CARGO_BUILD_TARGET = "arm-unknown-linux-musleabi";
    CARGO_TARGET_ARM_UNKNOWN_LINUX_MUSLEABI_LINKER = "${bmmCc}/bin/armv5tel-unknown-linux-musleabi-gcc";
    RUST_ANALYZER_SERVER_PATH = "${bmmRustToolchain}/bin/rust-analyzer";

    inherit (webuiEnvAttrs) UBOS_WEBUI_BUILD_REV UBOS_WEBUI_BUILD_TIME;

    # Resolved at shell-entry time relative to the user's cwd, which
    # by convention is the repository root, matching the existing
    # arm/esp shells using $PWD for state directories.
    shellHook = ''
      export CARGO_TARGET_ARM_UNKNOWN_LINUX_MUSLEABI_RUNNER="$PWD/host/host-runner/scripts/runner.py"
      echo "ubos-main dev environment ready (host-runner, BMM/miniminer cross)"
      echo ""
      echo "  cd host/host-runner && cargo build --release"
      echo "                         cargo run --release -- --hostname redacted50.1"
      echo ""
      echo "See host/host-runner/scripts/runner.py --help for more flags."
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
      ]
      ++ pkgs.lib.optionals (bench != null) [ deployRsPackage ];

    shellHook = ''
      echo "ubos-main — available development shells:"
      echo ""
      echo "  nix develop .#arm   — ARM boards (RP2040, RP2350, STM32F407, STM32F439)"
      echo "  nix develop .#esp   — ESP32-S3 board"
      echo "  nix develop .#host  — host-runner, native"
      echo "  nix develop .#bmm   — host-runner, BMM/miniminer cross"
      echo ""
      echo "This default shell provides picocom and usbutils."
    ''
    + pkgs.lib.optionalString (bench != null) ''
      echo "deploy-rs is included for NUC deployment."
    '';
  };
}
