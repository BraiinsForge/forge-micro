# Development shells for the eth-examples firmware.
#
# Why: each board needs a specific Rust target and, for the ESP32-S3,
# an Xtensa toolchain that is not in nixpkgs. This flake pins both so
# a clean checkout compiles without manual rustup or espup setup.
#
# What: `default` provides thumbv6m, thumbv7em, and thumbv8m.main targets
# plus probe-rs and debug tools for the W5500-EVB-Pico, W6100-EVB-Pico-2,
# DevEBox, and Nucleo-F439ZI boards. `esp32`
# installs a pinned espup release, patches its binaries for NixOS,
# and exposes espflash and probe-rs for the ESP32-S3.
#
# How: `nix develop` for the ARM boards, `nix develop .#esp32` for
# the ESP32-S3. Both shells print the relevant build commands on
# entry.
{
  description = "Bare-metal Ethernet examples";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    crane.url = "github:ipetkov/crane";
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
      rust-overlay,
      crane,
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs {
          inherit system overlays;
        };

        rustToolchain = pkgs.rust-bin.stable.latest.default.override {
          extensions = [
            "rust-src"
            "rust-analyzer"
            "rustfmt"
            "llvm-tools"
          ];
          targets = [
            "thumbv6m-none-eabi"
            "thumbv7em-none-eabihf"
            "thumbv8m.main-none-eabihf"
          ];
        };

        espRustToolchain = pkgs.rust-bin.stable.latest.default.override {
          extensions = [
            "rust-src"
            "rust-analyzer"
          ];
        };

        patchEspBinaries = pkgs.writeShellScriptBin "patch-esp-binaries" ''
          if [ -f /etc/NIXOS ] || [ ! -f /lib64/ld-linux-x86-64.so.2 ]; then
            echo "Patching ESP toolchain binaries for NixOS..."
            find "$RUSTUP_HOME/toolchains/esp" -type f | while read -r file; do
              if file "$file" 2>/dev/null | grep -q 'ELF'; then
                ${pkgs.patchelf}/bin/patchelf \
                  --set-interpreter "$(cat ${pkgs.stdenv.cc}/nix-support/dynamic-linker)" \
                  "$file" 2>/dev/null || true
                ${pkgs.patchelf}/bin/patchelf \
                  --set-rpath "${
                    pkgs.lib.makeLibraryPath [
                      pkgs.stdenv.cc.cc.lib
                      pkgs.glibc
                      pkgs.zlib
                      pkgs.libxml2
                      pkgs.openssl
                      pkgs.libffi
                    ]
                  }:$RUSTUP_HOME/toolchains/esp/lib:$(dirname "$file")" \
                  "$file" 2>/dev/null || true
              fi
            done
            echo "Patching complete."
          fi
        '';

        craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;

        src =
          let
            # Crane's default filter keeps Cargo.toml, Cargo.lock, and .rs files.
            # We also need .cargo/config.toml (per-crate target settings), linker
            # scripts (.x), and the vendored embassy-net-wiznet-patch directory.
            customFilter =
              path: _type:
              (builtins.match ".*\\.cargo/config\\.toml$" path != null)
              || (builtins.match ".*\\.x$" path != null)
              || (builtins.match ".*/embassy-net-wiznet-patch/.*" path != null);
          in
          pkgs.lib.cleanSourceWith {
            src = ./.;
            filter = path: type: (customFilter path type) || (craneLib.filterCargoSources path type);
          };
      in
      {
        checks = {
          clippy-rp2040 = craneLib.cargoClippy {
            inherit src;
            cargoArtifacts = null;
            cargoExtraArgs = "-p rp2040 --target thumbv6m-none-eabi";
            cargoClippyExtraArgs = "-- -D warnings";
          };

          clippy-rp2350 = craneLib.cargoClippy {
            inherit src;
            cargoArtifacts = null;
            cargoExtraArgs = "-p rp2350 --target thumbv8m.main-none-eabihf";
            cargoClippyExtraArgs = "-- -D warnings";
          };

          clippy-stm32f407 = craneLib.cargoClippy {
            inherit src;
            cargoArtifacts = null;
            cargoExtraArgs = "-p stm32f407 --target thumbv7em-none-eabihf";
            cargoClippyExtraArgs = "-- -D warnings";
          };

          clippy-stm32f439 = craneLib.cargoClippy {
            inherit src;
            cargoArtifacts = null;
            cargoExtraArgs = "-p stm32f439 --target thumbv7em-none-eabihf";
            cargoClippyExtraArgs = "-- -D warnings";
          };

          test = craneLib.cargoTest {
            inherit src;
            cargoArtifacts = null;
            cargoExtraArgs = "-p eth-examples";
          };
        };

        devShells = {
          default = pkgs.mkShell {
            buildInputs = with pkgs; [
              rustToolchain
              probe-rs-tools
              pkg-config
              libudev-zero
              picocom
              usbutils
            ];

            RUST_ANALYZER_SERVER_PATH = "${rustToolchain}/bin/rust-analyzer";

            shellHook = ''
              echo "eth-examples dev environment ready (ARM boards)"
              echo ""
              echo "  cd firmware/rp2040    && cargo build --release  # RP2040 W5500"
              echo "  cd firmware/rp2350    && cargo build --release  # RP2350 W6100"
              echo "  cd firmware/stm32f407 && cargo build --release  # STM32F407 LAN8720"
              echo "  cd firmware/stm32f439 && cargo build --release  # STM32F439 LAN8720"
              echo ""
              echo "For ESP32-S3 board: nix develop .#esp32"
            '';
          };

          esp32 = pkgs.mkShell {
            buildInputs = with pkgs; [
              espRustToolchain
              espup
              espflash
              probe-rs-tools
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
            ];

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

            shellHook = ''
              export RUSTUP_HOME="$PWD/.rustup"
              export CARGO_HOME="$PWD/.cargo"
              mkdir -p "$RUSTUP_HOME" "$CARGO_HOME"

              if [ ! -d "$RUSTUP_HOME/toolchains/esp/bin" ]; then
                # Remove partial state from a previous failed install.
                # espup aborts with "File exists" on pre-created LLVM
                # symlinks when the toolchain dir is present but incomplete.
                rm -rf "$RUSTUP_HOME/toolchains/esp"
                echo "Installing ESP32-S3 Xtensa toolchain..."
                # Version pinned for reproducibility. Bump manually when
                # upgrading esp-hal or esp-rtos and retest the build.
                espup install --targets esp32s3 --toolchain-version 1.94.0 || true
                patch-esp-binaries
              fi

              # Find GCC and clang paths dynamically to avoid hardcoding version strings.
              XTENSA_GCC_PATH="$(echo "$RUSTUP_HOME"/toolchains/esp/xtensa-esp-elf/esp-*/xtensa-esp-elf/bin | head -1)"
              LIBCLANG_TOOLCHAIN_PATH="$(echo "$RUSTUP_HOME"/toolchains/esp/xtensa-esp32-elf-clang/esp-*/esp-clang/lib | head -1)"

              export PATH="$XTENSA_GCC_PATH:$RUSTUP_HOME/toolchains/esp/bin:$CARGO_HOME/bin:$PATH"

              if [ -d "$LIBCLANG_TOOLCHAIN_PATH" ]; then
                export LIBCLANG_PATH="$LIBCLANG_TOOLCHAIN_PATH"
              fi

              echo "eth-examples dev environment ready (ESP32-S3)"
              echo ""
              echo "  cd firmware/esp32s3 && cargo build --release  # ESP32-S3 W5100S"
              echo ""
            '';
          };
        };
      }
    );
}
