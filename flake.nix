# Development shells for the eth-examples firmware.
#
# Why: each board needs a specific Rust target and, for the ESP32-S3,
# an Xtensa toolchain that is not in nixpkgs. This flake pins both so
# a clean checkout compiles without manual rustup or espup setup.
#
# What: `arm` provides thumbv6m, thumbv7em, and thumbv8m.main targets
# plus probe-rs and debug tools for the W5500-EVB-Pico, W6100-EVB-Pico-2,
# DevEBox, and Nucleo-F439ZI boards. `esp` installs a pinned espup
# release, patches its binaries for NixOS, and exposes espflash and
# probe-rs for the ESP32-S3. `default` provides NUC operations tools
# and lists available development shells.
#
# How: `nix develop .#arm` for the ARM boards, `nix develop .#esp` for
# the ESP32-S3. Both shells print the relevant build commands on entry.
{
  description = "Bare-metal Ethernet examples";

  # Binary caches for prebuilt Raspberry Pi kernel and deploy-rs.
  nixConfig = {
    extra-substituters = [
      "https://nixos-raspberrypi.cachix.org"
      "https://deploy-rs.cachix.org"
    ];
    extra-trusted-public-keys = [
      "nixos-raspberrypi.cachix.org-1:4iMO9LXa8BqhU+Rpg6LQKiGa2lsNh/j2oiYLNOQ5sPI="
      "deploy-rs.cachix.org-1:xfNobmiwF/vzvK1gpfediPwpdIP0rpDV2rYqx40zdSI="
    ];
  };

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    crane.url = "github:ipetkov/crane";
    # Pinned to PR#131 branch to share the nixos-raspberrypi binary cache and
    # avoid the nvmd/nixpkgs fork rebuild penalty. Revisit once the PR is
    # released on the upstream branch.
    nixos-raspberrypi.url = "github:nvmd/nixos-raspberrypi/remove-options-compat";
    deploy-rs.url = "github:serokell/deploy-rs";
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
      rust-overlay,
      crane,
      nixos-raspberrypi,
      deploy-rs,
    }@inputs:
    let
      nucHost = "redacted50.69";
      probeRsHost = "ws://${nucHost}:8080";
      probeRsToken = "redacted";

      probeRsOverlay = final: prev: {
        probe-rs-tools-remote = prev.probe-rs-tools.overrideAttrs (old: {
          cargoBuildFeatures = (old.cargoBuildFeatures or [ ]) ++ [ "remote" ];
        });
        probe-rs-runner = prev.writeShellScriptBin "probe-rs-runner" ''
          args=()
          for arg in "$@"; do
            if [ "$arg" = "--local" ]; then
              unset PROBE_RS_REMOTE_HOST PROBE_RS_REMOTE_TOKEN
            else
              args+=("$arg")
            fi
          done
          exec probe-rs "''${args[@]}"
        '';
      };

      # Build a NUC nixosSystem with the shared RPi overlays and platform
      # setting. Callers append image-specific modules (e.g. sd-image.nix).
      mkNuc =
        extraModules:
        nixpkgs.lib.nixosSystem {
          specialArgs = {
            inherit inputs nixos-raspberrypi;
          };
          modules = [
            nixos-raspberrypi.lib.inject-overlays
            nixos-raspberrypi.nixosModules.nixpkgs-rpi
            { nixpkgs.hostPlatform = "aarch64-linux"; }
            (
              { pkgs, ... }:
              {
                nixpkgs.overlays = [
                  probeRsOverlay
                  (final: prev: {
                    ckpool = final.callPackage ./redacted/pkgs/ckpool.nix { };
                  })
                ];
              }
            )
            ./redacted/nuc.nix
          ]
          ++ extraModules;
        };
      sdImage = (mkNuc [ ./redacted/sd-image.nix ]).config.system.build.sdImage;
    in
    (flake-utils.lib.eachDefaultSystem (
      system:
      let
        overlays = [
          (import rust-overlay)
          probeRsOverlay
        ];
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

          clippy-eth-examples = craneLib.cargoClippy {
            inherit src;
            cargoArtifacts = null;
            cargoExtraArgs = "-p eth-examples";
            cargoClippyExtraArgs = "-- -D warnings";
          };

          test = craneLib.cargoTest {
            inherit src;
            cargoArtifacts = null;
            cargoExtraArgs = "-p eth-examples";
          };

          # firmware/esp32s3 and echoer target Xtensa LX7. The toolchain
          # is installed via espup at devshell entry and is not packaged
          # in nixpkgs, so they cannot be wrapped in a flake check
          # derivation. Coverage relies on the developer running
          # `nix develop .#esp` and `cargo build` before merge.
        };

        devShells = {
          arm = pkgs.mkShell {
            buildInputs =
              with pkgs;
              [
                rustToolchain
                probe-rs-tools-remote
                probe-rs-runner
                pkg-config
                libudev-zero
                picocom
                usbutils
              ]
              ++ [ deploy-rs.packages.${system}.default ];

            PROBE_RS_REMOTE_HOST = probeRsHost;
            PROBE_RS_REMOTE_TOKEN = probeRsToken;
            RUST_ANALYZER_SERVER_PATH = "${rustToolchain}/bin/rust-analyzer";

            shellHook = ''
              echo "eth-examples dev environment ready (ARM boards)"
              echo ""
              echo "  cd firmware/rp2040    && cargo build --release  # RP2040 W5500"
              echo "  cd firmware/rp2350    && cargo build --release  # RP2350 W6100"
              echo "  cd firmware/stm32f407 && cargo build --release  # STM32F407 LAN8720"
              echo "  cd firmware/stm32f439 && cargo build --release  # STM32F439 LAN8720"
              echo ""
              echo "For ESP32-S3 board: nix develop .#esp"
              echo "Deploy to NUC:      deploy .#nuc"
            '';
          };

          esp = pkgs.mkShell {
            buildInputs = [
              pkgs.probe-rs-tools-remote
              pkgs.probe-rs-runner
            ]
            ++ (with pkgs; [
              espRustToolchain
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

            PROBE_RS_REMOTE_HOST = probeRsHost;
            PROBE_RS_REMOTE_TOKEN = probeRsToken;
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
                # espup also writes a global $HOME/.espup/esp-clang symlink
                # pointing at the last-installed toolchain. A stale link
                # left by another worktree makes espup abort with EEXIST
                # even though no local state remains.
                rm -f "$HOME/.espup/esp-clang"
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
              echo "For ARM boards: nix develop .#arm"
            '';
          };

          default = pkgs.mkShell {
            buildInputs =
              with pkgs;
              [
                picocom
                usbutils
              ]
              ++ [ deploy-rs.packages.${system}.default ];

            shellHook = ''
              echo "eth-examples — available development shells:"
              echo ""
              echo "  nix develop .#arm  — ARM boards (RP2040, RP2350, STM32F407, STM32F439)"
              echo "  nix develop .#esp  — ESP32-S3 board"
              echo ""
              echo "This default shell provides deploy-rs, picocom, and usbutils"
              echo "for NUC operations."
            '';
          };
        };
      }
    ))
    // {
      # Raspberry Pi 4 support node that connects the target boards to the
      # redacted mesh. See redacted/nuc.nix for the host definition.
      nixosConfigurations.nuc = mkNuc [ ];

      # Remote deployment via deploy-rs. Pushes the NUC NixOS closure over
      # SSH and activates it without requiring a full SD-card reflash.
      deploy.nodes.nuc = {
        hostname = nucHost;
        sshUser = "admin";
        profiles.system = {
          user = "root";
          path = deploy-rs.lib.aarch64-linux.activate.nixos self.nixosConfigurations.nuc;
        };
      };

      # Bootable SD-card image for the NUC. The image is always aarch64 but
      # the build can run on any host via binfmt/QEMU. Build with:
      #   redacted_SETUP_KEY=<key> \
      #   WIFI_NETWORKS='[{"ssid":"home","psk":"secret"}]' \
      #     nix build .#sd-image --impure
      packages.x86_64-linux.sd-image = sdImage;
      packages.aarch64-linux.sd-image = sdImage;
    };
}
