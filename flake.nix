# Development shells for the ubos-main firmware.
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
  description = "ubos-main bare-metal Rust firmware workspace";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    crane.url = "github:ipetkov/crane";
    # Pinned to PR#131 branch to avoid the nvmd/nixpkgs fork rebuild penalty
    # and to match cache paths for users who opt into the nixos-raspberrypi
    # cachix entry locally. Revisit once the PR is released upstream.
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

      # `serve` panics on `ConnectionClosed` when the client disconnects
      # mid-RPC, leaving the probe Session held but the worker dead.
      # systemd does not restart because the panic is tokio-contained,
      # so the next client gets a silent exit-0. No upstream fix yet.
      mkRemoteProbeRsTools =
        base:
        base.overrideAttrs (old: {
          cargoBuildFeatures = (old.cargoBuildFeatures or [ ]) ++ [ "remote" ];
          patches = (old.patches or [ ]) ++ [
            ./patches/probe-rs-rpc-tolerate-disconnect.patch
          ];
        });

      probeRsOverlay = final: prev: {
        probe-rs-tools-remote = mkRemoteProbeRsTools prev.probe-rs-tools;
        probe-rs-runner = prev.writeShellScriptBin "probe-rs-runner" ''
          args=()
          for arg in "$@"; do
            if [ "$arg" = "--local" ]; then
              unset PROBE_RS_REMOTE_HOST PROBE_RS_REMOTE_TOKEN
            else
              args+=("$arg")
            fi
          done
          # Default RUST_LOG=info so probe-rs surfaces session errors
          # to stderr instead of returning silently with exit 0 when a
          # CMSIS-DAP probe is wedged (probe-rs/probe-rs#1849).
          # Caller-set RUST_LOG wins.
          export RUST_LOG="''${RUST_LOG:-info}"
          start=$(date +%s)
          probe-rs "''${args[@]}"
          status=$?
          elapsed=$(( $(date +%s) - start ))
          # A real flash plus attach of any of our targets takes longer
          # than this threshold. An exit-0 in under that with `run`
          # requested means probe-rs short-circuited without ever
          # talking to the chip - canonically the wedged CMSIS-DAP case
          # where the remote server swallows the error. Surface a hint
          # so the failure isn't silent.
          min_flash_seconds=4
          if [ "$status" -eq 0 ] && [ "$elapsed" -lt "$min_flash_seconds" ]; then
            for arg in "''${args[@]}"; do
              if [ "$arg" = "run" ] || [ "$arg" = "attach" ]; then
                echo "probe-rs-runner: probe-rs exited 0 in ''${elapsed}s without flashing - probe likely wedged (probe-rs/probe-rs#1849 for CMSIS-DAP)." >&2
                echo "  Physically replug the probe; check 'journalctl -u probe-rs-server' on the NUC for server-side errors." >&2
                break
              fi
            done
          fi
          if [ "$status" -ne 0 ]; then
            echo "probe-rs-runner: probe-rs exited $status" >&2
          fi
          exit $status
        '';
      };

      # Build a NUC nixosSystem with the shared RPi overlays and platform
      # setting. Callers append image-specific modules (e.g. sd-image.nix).
      # buildSystem is the system doing the build. On aarch64-linux the
      # aarch64 packages build natively; on other systems they go through
      # pkgsCross.aarch64-multiplatform so rustc stays native rather than
      # running under binfmt/QEMU.
      mkNuc =
        buildSystem: extraModules:
        let
          aarch64Pkgs =
            if buildSystem == "aarch64-linux" then
              import nixpkgs { system = "aarch64-linux"; }
            else
              (import nixpkgs { system = buildSystem; }).pkgsCross.aarch64-multiplatform;
        in
        nixpkgs.lib.nixosSystem {
          specialArgs = {
            inherit inputs nixos-raspberrypi nucHost;
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
                  # Override probe-rs-tools-remote with a cross-compiled
                  # build so rustc runs natively on the build host instead
                  # of under QEMU. The probe-rs-runner shell wrapper from
                  # probeRsOverlay above still applies unchanged.
                  (final: _prev: {
                    probe-rs-tools-remote = mkRemoteProbeRsTools aarch64Pkgs.probe-rs-tools;
                  })
                  (final: _prev: {
                    load-test = aarch64Pkgs.callPackage ./nuc/pkgs/load-test.nix { };
                  })
                ];
              }
            )
            ./nuc/nuc.nix
          ]
          ++ extraModules;
        };
      sdImageFor = buildSystem: (mkNuc buildSystem [ ./nuc/sd-image.nix ]).config.system.build.sdImage;
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

        hostRustToolchain = pkgs.rust-bin.stable.latest.default.override {
          extensions = [
            "rust-src"
            "rust-analyzer"
            "rustfmt"
            "llvm-tools"
          ];
        };

        bmmRustToolchain = pkgs.rust-bin.stable.latest.default.override {
          extensions = [
            "rust-src"
            "rust-analyzer"
            "rustfmt"
            "llvm-tools"
          ];
          targets = [ "arm-unknown-linux-musleabi" ];
        };

        # Cross gcc for the miniminer (BMM) target. Rust emits the code,
        # gcc only links, so a generic ARM-musleabi toolchain works for
        # the Cortex-A9 device. The crossSystem is pinned to armv5tel
        # because that is the baseline soft-float ARMv5 musl variant that
        # matches Rust's `arm-unknown-linux-musleabi` ABI; nixpkgs rejects
        # the unversioned `arm-linux` system because musl's meta.platforms
        # lists only versioned variants. ARMv5 binaries run unchanged on
        # the Cortex-A9 (ARMv7l). Off-Nix workflows that use the OpenWrt
        # SDK directly are unaffected; that path is user-local.
        pkgsBmm = import nixpkgs {
          inherit system;
          crossSystem = {
            config = "armv5tel-unknown-linux-musleabi";
          };
        };
        bmmCc = pkgsBmm.stdenv.cc;

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

        bmmCraneLib = (crane.mkLib pkgs).overrideToolchain bmmRustToolchain;

        # Xtensa-LX7 Rust toolchain for the ESP32-S3. Upstream rustc does
        # not ship xtensa-esp32s3-none-elf; esp-rs publishes pre-built
        # artifacts at github.com/esp-rs/rust-build. Running espup inside
        # a Nix build is non-deterministic, so flake checks fetch the
        # release tarballs directly with pinned hashes.
        #
        # When bumping espToolchainVersion:
        #   nix-prefetch-url <rust tarball url>
        #   nix-prefetch-url <rust-src tarball url>
        #   nix hash convert --to sri --hash-algo sha256 <each base32>
        # The --unpack flag is intentionally omitted; fetchurl verifies
        # the compressed tarball directly.
        # Update the hashes below and retest both flake checks and the
        # esp devshell in the same commit.
        espToolchainVersion = "1.94.0.2";

        espToolchain = pkgs.stdenv.mkDerivation {
          pname = "esp-rust-xtensa";
          version = espToolchainVersion;

          srcs = [
            (pkgs.fetchurl {
              url = "https://github.com/esp-rs/rust-build/releases/download/v${espToolchainVersion}/rust-${espToolchainVersion}-x86_64-unknown-linux-gnu.tar.xz";
              hash = "sha256-bOG2jBJtHuDBfUWejEg7kXaAAfzpzCz4lfbGiMMmKxU=";
            })
            (pkgs.fetchurl {
              url = "https://github.com/esp-rs/rust-build/releases/download/v${espToolchainVersion}/rust-src-${espToolchainVersion}.tar.xz";
              hash = "sha256-A8J1ScXW8c39Tw/6KKYsC6+XnCgkGewSGnfxttKxz4c=";
            })
          ];

          nativeBuildInputs = [ pkgs.autoPatchelfHook ];
          buildInputs = [
            pkgs.stdenv.cc.cc
            pkgs.zlib
          ];

          sourceRoot = ".";
          dontStrip = true;

          # Both tarballs unpack to directories named rust-nightly-*
          # regardless of the release version, because esp-rs builds
          # from the upstream rust master branch.
          installPhase = ''
            runHook preInstall
            for dir in rust-nightly-x86_64-unknown-linux-gnu rust-src-nightly; do
              patchShebangs "$dir/install.sh"
              "$dir/install.sh" \
                --destdir=$out --prefix="" --disable-ldconfig \
                --without=rust-docs-json-preview,rust-docs
            done
            runHook postInstall
          '';
        };

        espCraneLib = (crane.mkLib pkgs).overrideToolchain espToolchain;

        # Xtensa GCC toolchain for `secp256k1-sys` (and any other C
        # dependency in the esp32s3 build). `secp256k1-sys` arrives via
        # the unconditional `ii-sv2` dep. Nixpkgs doesn't ship
        # `xtensa-esp-elf-gcc`, and the rustup-managed copy from `espup`
        # is invisible inside the flake check sandbox. Hashes paired with
        # the URL; bump both when upgrading to a newer
        # espressif/crosstool-NG release.
        xtensaEspToolchain = pkgs.stdenv.mkDerivation {
          pname = "xtensa-esp-elf";
          version = "15.2.0_20250920";

          src = pkgs.fetchurl {
            url = "https://github.com/espressif/crosstool-NG/releases/download/esp-15.2.0_20250920/xtensa-esp-elf-15.2.0_20250920-x86_64-linux-gnu.tar.xz";
            hash = "sha256-49d60UVEgUUnu+ei0PeexFkqTiM5LFHHOIwOaGtqaXc=";
          };

          nativeBuildInputs = [ pkgs.autoPatchelfHook ];
          buildInputs = [
            pkgs.stdenv.cc.cc
            pkgs.zlib
          ];

          sourceRoot = "xtensa-esp-elf";
          dontStrip = true;

          installPhase = ''
            runHook preInstall
            mkdir -p "$out"
            cp -r . "$out"
            runHook postInstall
          '';
        };

        src =
          let
            # Crane's default filter keeps Cargo.toml, Cargo.lock, and .rs files.
            # We also need .cargo/config.toml (per-crate target settings), linker
            # scripts (.x), the vendored embassy-net-wiznet patches directory,
            # the dashboard HTML that ubos-webui's build.rs gzips into OUT_DIR,
            # the Braiins wordmark served at /logo.svg via include_bytes!, and
            # the build.rs sibling at the ubos-webui crate root.
            customFilter =
              path: _type:
              (builtins.match ".*\\.cargo/config\\.toml$" path != null)
              || (builtins.match ".*\\.x$" path != null)
              || (builtins.match ".*/patches/embassy-net-wiznet/.*" path != null)
              || (builtins.match ".*\\.html$" path != null)
              || (builtins.match ".*\\.svg$" path != null)
              || (baseNameOf path == "build.rs");
          in
          pkgs.lib.cleanSourceWith {
            src = ./.;
            filter = path: type: (customFilter path type) || (craneLib.filterCargoSources path type);
          };

        # build-std=core pulls sysroot crates like proc_macro and std
        # that have their own Cargo.lock outside our tree. Vendor both
        # so the Xtensa clippy checks resolve without network access.
        espCargoVendorDir = espCraneLib.vendorMultipleCargoDeps {
          cargoLockList = [
            ./Cargo.lock
            "${espToolchain}/lib/rustlib/src/rust/library/Cargo.lock"
          ];
        };

        # Host-target load-test crate lives outside the ubos-main
        # workspace, so it gets its own src filter and dep cache rather
        # than sharing the embedded workspace wiring above.
        loadTestSrc = craneLib.cleanCargoSource ./nuc/pkgs/load-test;
        loadTestArtifacts = craneLib.buildDepsOnly {
          src = loadTestSrc;
          strictDeps = true;
        };

        # Shared dependency artifacts for clippy checks. Flake checks are
        # per-crate; without reused artifacts each check recompiles the full
        # dependency graph and dominates `nix flake check` wall time.
        #
        # The bags below split host crates from firmware crates because cargo
        # unifies features within one invocation and the two groups enable
        # mutually exclusive ones such as `embassy-time/defmt` versus
        # `embassy-time/log`. Firmware bags also split per target, since
        # crane keys deps-only artifacts by `--target`.
        hostArtifacts = craneLib.buildDepsOnly {
          inherit src;
          doCheck = false;
          cargoBuildCommand = "true";
          cargoExtraArgs = builtins.concatStringsSep " " [
            "-p embassy-net-afpacket"
            "-p embedded-i2c-unix"
            "-p embedded-serial-unix"
            "-p mining-protocol"
            "-p serial-baudrate"
            "-p stratum-client"
            "-p stratum-v1"
            "-p stratum-v2"
            "-p ubos-auth"
            "-p ubos-core"
            "-p ubos-display"
            "-p ubos-logging"
            "-p ubos-main-host"
            "-p ubos-metrics"
            "-p ubos-mining"
            "-p ubos-net"
            "-p ubos-pool"
            "-p ubos-psu"
            "-p ubos-temperature"
          ];
        };

        hostRunnerArtifacts = craneLib.buildDepsOnly {
          inherit src;
          doCheck = false;
          cargoBuildCommand = "true";
          cargoExtraArgs = "-p host-runner";
        };

        ubosThermalArtifacts = craneLib.buildDepsOnly {
          inherit src;
          doCheck = false;
          cargoBuildCommand = "true";
          cargoExtraArgs = "-p ubos-thermal";
        };

        ubosConfigArtifacts = craneLib.buildDepsOnly {
          inherit src;
          doCheck = false;
          cargoBuildCommand = "true";
          cargoExtraArgs = "-p ubos-config";
        };

        ubosConfigStdArtifacts = craneLib.buildDepsOnly {
          inherit src;
          doCheck = false;
          cargoBuildCommand = "true";
          cargoExtraArgs = "-p ubos-config --features std";
        };

        ubosCoolingArtifacts = craneLib.buildDepsOnly {
          inherit src;
          doCheck = false;
          cargoBuildCommand = "true";
          cargoExtraArgs = "-p ubos-cooling";
        };

        ubosManagerArtifacts = craneLib.buildDepsOnly {
          inherit src;
          doCheck = false;
          cargoBuildCommand = "true";
          cargoExtraArgs = "-p ubos-manager";
        };

        thumbv6mArtifacts = craneLib.buildDepsOnly {
          inherit src;
          doCheck = false;
          cargoBuildCommand = "true";
          cargoExtraArgs = "-p rp2040 --target thumbv6m-none-eabi";
          nativeBuildInputs = [ pkgs.gcc-arm-embedded ];
          CC_thumbv6m_none_eabi = "${pkgs.gcc-arm-embedded}/bin/arm-none-eabi-gcc";
        };

        # stm32f407 and stm32f439 cannot share a deps-only bag: each selects a
        # different chip feature on `stm32-metapac`, and cargo's per-invocation
        # feature unification activates both, which the metapac build script
        # rejects with "Multiple stm32xx Cargo features enabled".
        stm32f407Artifacts = craneLib.buildDepsOnly {
          inherit src;
          doCheck = false;
          cargoBuildCommand = "true";
          cargoExtraArgs = "-p stm32f407 --target thumbv7em-none-eabihf";
          nativeBuildInputs = [ pkgs.gcc-arm-embedded ];
          CC_thumbv7em_none_eabihf = "${pkgs.gcc-arm-embedded}/bin/arm-none-eabi-gcc";
        };

        stm32f439Artifacts = craneLib.buildDepsOnly {
          inherit src;
          doCheck = false;
          cargoBuildCommand = "true";
          cargoExtraArgs = "-p stm32f439 --target thumbv7em-none-eabihf";
          nativeBuildInputs = [ pkgs.gcc-arm-embedded ];
          CC_thumbv7em_none_eabihf = "${pkgs.gcc-arm-embedded}/bin/arm-none-eabi-gcc";
        };

        thumbv8mArtifacts = craneLib.buildDepsOnly {
          inherit src;
          doCheck = false;
          cargoBuildCommand = "true";
          cargoExtraArgs = builtins.concatStringsSep " " [
            "-p bmm-pico"
            "-p rp2350"
            "-p rp235x-sha256"
            "--target thumbv8m.main-none-eabihf"
          ];
          nativeBuildInputs = [ pkgs.gcc-arm-embedded ];
          CC_thumbv8m_main_none_eabihf = "${pkgs.gcc-arm-embedded}/bin/arm-none-eabi-gcc";
        };

        bmmArtifacts = bmmCraneLib.buildDepsOnly {
          inherit src;
          doCheck = false;
          cargoBuildCommand = "true";
          cargoExtraArgs = "-p host-runner --target arm-unknown-linux-musleabi";
          nativeBuildInputs = [ bmmCc ];
          CARGO_TARGET_ARM_UNKNOWN_LINUX_MUSLEABI_LINKER = "${bmmCc}/bin/armv5tel-unknown-linux-musleabi-gcc";
          CC_arm_unknown_linux_musleabi = "${bmmCc}/bin/armv5tel-unknown-linux-musleabi-gcc";
        };

        espArtifacts = espCraneLib.buildDepsOnly {
          inherit src;
          doCheck = false;
          cargoBuildCommand = "true";
          cargoVendorDir = espCargoVendorDir;
          cargoExtraArgs = builtins.concatStringsSep " " [
            "-p esp32s3@0.1.0"
            "-p esp32s3-tsens"
            "--target xtensa-esp32s3-none-elf"
            "-Z build-std=core"
          ];
          RUST_SRC_PATH = "${espToolchain}/lib/rustlib/src/rust/library";
          nativeBuildInputs = [ xtensaEspToolchain ];
          CC_xtensa_esp32s3_none_elf = "${xtensaEspToolchain}/bin/xtensa-esp32s3-elf-gcc";
          CFLAGS_xtensa_esp32s3_none_elf = "-mlongcalls";
        };
      in
      {
        # Each firmware crate is checked under its default feature set
        # only.
        #
        # `patches/edge-nal-embassy` is intentionally not gated by its
        # own clippy or build derivation. It is a vendored upstream
        # crate carrying a local fix that we do not author; gating it
        # in CI would tie the build to upstream lint cleanliness we do
        # not control. Every firmware build derivation below exercises
        # it transitively, so a real breakage still fails the gate.
        checks = {
          clippy-bmm-pico = craneLib.cargoClippy {
            inherit src;
            cargoArtifacts = thumbv8mArtifacts;
            cargoExtraArgs = "-p bmm-pico --target thumbv8m.main-none-eabihf";
            cargoClippyExtraArgs = "-- -D warnings";
            doInstallCargoArtifacts = false;
            # `secp256k1-sys` is an unconditional dep via `ii-sv2` and
            # its C build script requires a cross compiler. `cc-rs`
            # otherwise falls back to host `gcc` and fails on the
            # thumb-specific flags; point it at arm-none-eabi-gcc.
            nativeBuildInputs = [ pkgs.gcc-arm-embedded ];
            CC_thumbv8m_main_none_eabihf = "${pkgs.gcc-arm-embedded}/bin/arm-none-eabi-gcc";
          };

          # Release build of each firmware binary. cargo check and
          # cargo clippy stop short of linking the final ELF for
          # binary crates, so they miss .bss / flash overflows. This
          # derivation invokes the full linker pass so any RAM or
          # flash budget regression fails CI at the same gate as a
          # missing trait bound. The Footprint section of
          # docs/STRUCTURE.md records the per-board RAM snapshot this
          # gate protects.
          build-bmm-pico = craneLib.cargoBuild {
            inherit src;
            cargoArtifacts = null;
            cargoBuildCommand = "cargo build --release";
            cargoExtraArgs = "-p bmm-pico --target thumbv8m.main-none-eabihf";
            nativeBuildInputs = [ pkgs.gcc-arm-embedded ];
            CC_thumbv8m_main_none_eabihf = "${pkgs.gcc-arm-embedded}/bin/arm-none-eabi-gcc";
          };

          clippy-rp2040 = craneLib.cargoClippy {
            inherit src;
            cargoArtifacts = thumbv6mArtifacts;
            cargoExtraArgs = "-p rp2040 --target thumbv6m-none-eabi";
            cargoClippyExtraArgs = "-- -D warnings";
            doInstallCargoArtifacts = false;
            # `secp256k1-sys` is an unconditional dep via `ii-sv2` and
            # its C build script requires a cross compiler. Point `cc-rs`
            # at the arm-none-eabi cross-gcc so the C code is built
            # against the M0+ ABI.
            nativeBuildInputs = [ pkgs.gcc-arm-embedded ];
            CC_thumbv6m_none_eabi = "${pkgs.gcc-arm-embedded}/bin/arm-none-eabi-gcc";
          };

          build-rp2040 = craneLib.cargoBuild {
            inherit src;
            cargoArtifacts = null;
            cargoBuildCommand = "cargo build --release";
            cargoExtraArgs = "-p rp2040 --target thumbv6m-none-eabi";
            nativeBuildInputs = [ pkgs.gcc-arm-embedded ];
            CC_thumbv6m_none_eabi = "${pkgs.gcc-arm-embedded}/bin/arm-none-eabi-gcc";
          };

          clippy-rp2350 = craneLib.cargoClippy {
            inherit src;
            cargoArtifacts = thumbv8mArtifacts;
            cargoExtraArgs = "-p rp2350 --target thumbv8m.main-none-eabihf";
            cargoClippyExtraArgs = "-- -D warnings";
            doInstallCargoArtifacts = false;
            # `secp256k1-sys` is an unconditional dep via `ii-sv2` and
            # its C build script requires a cross compiler. `cc-rs`
            # otherwise falls back to host `gcc` and fails on the
            # thumb-specific flags; point it at arm-none-eabi-gcc.
            nativeBuildInputs = [ pkgs.gcc-arm-embedded ];
            CC_thumbv8m_main_none_eabihf = "${pkgs.gcc-arm-embedded}/bin/arm-none-eabi-gcc";
          };

          build-rp2350 = craneLib.cargoBuild {
            inherit src;
            cargoArtifacts = null;
            cargoBuildCommand = "cargo build --release";
            cargoExtraArgs = "-p rp2350 --target thumbv8m.main-none-eabihf";
            nativeBuildInputs = [ pkgs.gcc-arm-embedded ];
            CC_thumbv8m_main_none_eabihf = "${pkgs.gcc-arm-embedded}/bin/arm-none-eabi-gcc";
          };

          clippy-rp235x-sha256 = craneLib.cargoClippy {
            inherit src;
            cargoArtifacts = thumbv8mArtifacts;
            cargoExtraArgs = "-p rp235x-sha256 --target thumbv8m.main-none-eabihf";
            cargoClippyExtraArgs = "-- -D warnings";
            doInstallCargoArtifacts = false;
          };

          clippy-stm32f407 = craneLib.cargoClippy {
            inherit src;
            cargoArtifacts = stm32f407Artifacts;
            cargoExtraArgs = "-p stm32f407 --target thumbv7em-none-eabihf";
            cargoClippyExtraArgs = "-- -D warnings";
            doInstallCargoArtifacts = false;
            # `secp256k1-sys` is an unconditional dep via `ii-sv2` and
            # its C build script requires a cross compiler. Point `cc-rs`
            # at the arm-none-eabi cross-gcc so the C code is built
            # against the correct ABI.
            nativeBuildInputs = [ pkgs.gcc-arm-embedded ];
            CC_thumbv7em_none_eabihf = "${pkgs.gcc-arm-embedded}/bin/arm-none-eabi-gcc";
          };

          build-stm32f407 = craneLib.cargoBuild {
            inherit src;
            cargoArtifacts = null;
            cargoBuildCommand = "cargo build --release";
            cargoExtraArgs = "-p stm32f407 --target thumbv7em-none-eabihf";
            nativeBuildInputs = [ pkgs.gcc-arm-embedded ];
            CC_thumbv7em_none_eabihf = "${pkgs.gcc-arm-embedded}/bin/arm-none-eabi-gcc";
          };

          clippy-stm32f439 = craneLib.cargoClippy {
            inherit src;
            cargoArtifacts = stm32f439Artifacts;
            cargoExtraArgs = "-p stm32f439 --target thumbv7em-none-eabihf";
            cargoClippyExtraArgs = "-- -D warnings";
            doInstallCargoArtifacts = false;
            # `secp256k1-sys` is an unconditional dep via `ii-sv2` and
            # its C build script requires a cross compiler. Point `cc-rs`
            # at the arm-none-eabi cross-gcc so the C code is built
            # against the correct ABI.
            nativeBuildInputs = [ pkgs.gcc-arm-embedded ];
            CC_thumbv7em_none_eabihf = "${pkgs.gcc-arm-embedded}/bin/arm-none-eabi-gcc";
          };

          build-stm32f439 = craneLib.cargoBuild {
            inherit src;
            cargoArtifacts = null;
            cargoBuildCommand = "cargo build --release";
            cargoExtraArgs = "-p stm32f439 --target thumbv7em-none-eabihf";
            nativeBuildInputs = [ pkgs.gcc-arm-embedded ];
            CC_thumbv7em_none_eabihf = "${pkgs.gcc-arm-embedded}/bin/arm-none-eabi-gcc";
          };

          clippy-ubos-core = craneLib.cargoClippy {
            inherit src;
            cargoArtifacts = hostArtifacts;
            cargoExtraArgs = "-p ubos-core";
            cargoClippyExtraArgs = "-- -D warnings";
            doInstallCargoArtifacts = false;
          };

          clippy-ubos-http = craneLib.cargoClippy {
            inherit src;
            cargoArtifacts = hostArtifacts;
            cargoExtraArgs = "-p ubos-http";
            cargoClippyExtraArgs = "-- -D warnings";
            doInstallCargoArtifacts = false;
          };

          clippy-ubos-api = craneLib.cargoClippy {
            inherit src;
            cargoArtifacts = hostArtifacts;
            cargoExtraArgs = "-p ubos-api";
            cargoClippyExtraArgs = "-- -D warnings";
            doInstallCargoArtifacts = false;
          };

          clippy-ubos-auth = craneLib.cargoClippy {
            inherit src;
            cargoArtifacts = hostArtifacts;
            cargoExtraArgs = "-p ubos-auth";
            cargoClippyExtraArgs = "-- -D warnings";
            doInstallCargoArtifacts = false;
          };

          clippy-ubos-metrics = craneLib.cargoClippy {
            inherit src;
            cargoArtifacts = hostArtifacts;
            cargoExtraArgs = "-p ubos-metrics";
            cargoClippyExtraArgs = "-- -D warnings";
            doInstallCargoArtifacts = false;
          };

          clippy-ubos-webui = craneLib.cargoClippy {
            inherit src;
            cargoArtifacts = hostArtifacts;
            cargoExtraArgs = "-p ubos-webui";
            cargoClippyExtraArgs = "-- -D warnings";
            doInstallCargoArtifacts = false;
          };

          clippy-ubos-display = craneLib.cargoClippy {
            inherit src;
            cargoArtifacts = hostArtifacts;
            cargoExtraArgs = "-p ubos-display";
            cargoClippyExtraArgs = "-- -D warnings";
            doInstallCargoArtifacts = false;
          };

          clippy-ubos-mdns = craneLib.cargoClippy {
            inherit src;
            cargoArtifacts = hostArtifacts;
            cargoExtraArgs = "-p ubos-mdns";
            cargoClippyExtraArgs = "-- -D warnings";
            doInstallCargoArtifacts = false;
          };

          clippy-ubos-net = craneLib.cargoClippy {
            inherit src;
            cargoArtifacts = hostArtifacts;
            cargoExtraArgs = "-p ubos-net";
            cargoClippyExtraArgs = "-- -D warnings";
            doInstallCargoArtifacts = false;
          };

          test-ubos-net = craneLib.cargoTest {
            inherit src;
            cargoArtifacts = hostArtifacts;
            cargoExtraArgs = "-p ubos-net";
            doInstallCargoArtifacts = false;
          };

          clippy-ubos-pool = craneLib.cargoClippy {
            inherit src;
            cargoArtifacts = hostArtifacts;
            cargoExtraArgs = "-p ubos-pool";
            cargoClippyExtraArgs = "-- -D warnings";
            doInstallCargoArtifacts = false;
          };

          clippy-ubos-logging = craneLib.cargoClippy {
            inherit src;
            cargoArtifacts = hostArtifacts;
            cargoExtraArgs = "-p ubos-logging";
            cargoClippyExtraArgs = "-- -D warnings";
            doInstallCargoArtifacts = false;
          };

          clippy-host-runner-host = craneLib.cargoClippy {
            inherit src;
            cargoArtifacts = hostRunnerArtifacts;
            cargoExtraArgs = "-p host-runner";
            cargoClippyExtraArgs = "-- -D warnings";
            doInstallCargoArtifacts = false;
          };

          test-host-runner = craneLib.cargoTest {
            inherit src;
            cargoArtifacts = hostRunnerArtifacts;
            cargoExtraArgs = "-p host-runner";
            doInstallCargoArtifacts = false;
          };

          clippy-host-runner-bmm = bmmCraneLib.cargoClippy {
            inherit src;
            cargoArtifacts = bmmArtifacts;
            cargoExtraArgs = "-p host-runner --target arm-unknown-linux-musleabi";
            cargoClippyExtraArgs = "-- -D warnings";
            doInstallCargoArtifacts = false;
            nativeBuildInputs = [ bmmCc ];
            CARGO_TARGET_ARM_UNKNOWN_LINUX_MUSLEABI_LINKER = "${bmmCc}/bin/armv5tel-unknown-linux-musleabi-gcc";
            # SV2 pulls in `secp256k1-sys`, whose C build script defers to
            # `cc-rs`. Point it at the same musleabi cross-gcc the linker
            # uses so the C objects share the target ABI.
            CC_arm_unknown_linux_musleabi = "${bmmCc}/bin/armv5tel-unknown-linux-musleabi-gcc";
          };

          clippy-embassy-net-afpacket = craneLib.cargoClippy {
            inherit src;
            cargoArtifacts = hostArtifacts;
            cargoExtraArgs = "-p embassy-net-afpacket";
            cargoClippyExtraArgs = "-- -D warnings";
            doInstallCargoArtifacts = false;
          };

          clippy-embedded-serial-unix = craneLib.cargoClippy {
            inherit src;
            cargoArtifacts = hostArtifacts;
            cargoExtraArgs = "-p embedded-serial-unix";
            cargoClippyExtraArgs = "-- -D warnings";
            doInstallCargoArtifacts = false;
          };

          clippy-ubos-psu = craneLib.cargoClippy {
            inherit src;
            cargoArtifacts = hostArtifacts;
            cargoExtraArgs = "-p ubos-psu";
            cargoClippyExtraArgs = "-- -D warnings";
            doInstallCargoArtifacts = false;
          };

          test-ubos-psu = craneLib.cargoTest {
            inherit src;
            cargoArtifacts = hostArtifacts;
            cargoExtraArgs = "-p ubos-psu";
            doInstallCargoArtifacts = false;
          };

          clippy-embedded-i2c-unix = craneLib.cargoClippy {
            inherit src;
            cargoArtifacts = hostArtifacts;
            cargoExtraArgs = "-p embedded-i2c-unix";
            cargoClippyExtraArgs = "-- -D warnings";
            doInstallCargoArtifacts = false;
          };

          test-embedded-i2c-unix = craneLib.cargoTest {
            inherit src;
            cargoArtifacts = hostArtifacts;
            cargoExtraArgs = "-p embedded-i2c-unix";
            doInstallCargoArtifacts = false;
          };

          clippy-ubos-mining = craneLib.cargoClippy {
            inherit src;
            cargoArtifacts = hostArtifacts;
            cargoExtraArgs = "-p ubos-mining";
            cargoClippyExtraArgs = "-- -D warnings";
            doInstallCargoArtifacts = false;
          };

          clippy-serial-baudrate = craneLib.cargoClippy {
            inherit src;
            cargoArtifacts = hostArtifacts;
            cargoExtraArgs = "-p serial-baudrate";
            cargoClippyExtraArgs = "-- -D warnings";
            doInstallCargoArtifacts = false;
          };

          clippy-mining-protocol = craneLib.cargoClippy {
            inherit src;
            cargoArtifacts = hostArtifacts;
            cargoExtraArgs = "-p mining-protocol";
            cargoClippyExtraArgs = "-- -D warnings";
            doInstallCargoArtifacts = false;
          };

          clippy-stratum-v1 = craneLib.cargoClippy {
            inherit src;
            cargoArtifacts = hostArtifacts;
            cargoExtraArgs = "-p stratum-v1";
            cargoClippyExtraArgs = "-- -D warnings";
            doInstallCargoArtifacts = false;
          };

          clippy-stratum-v2 = craneLib.cargoClippy {
            inherit src;
            cargoArtifacts = hostArtifacts;
            cargoExtraArgs = "-p stratum-v2";
            cargoClippyExtraArgs = "-- -D warnings";
            doInstallCargoArtifacts = false;
          };

          clippy-stratum-client = craneLib.cargoClippy {
            inherit src;
            cargoArtifacts = hostArtifacts;
            cargoExtraArgs = "-p stratum-client";
            cargoClippyExtraArgs = "-- -D warnings";
            doInstallCargoArtifacts = false;
          };

          clippy-ubos-main-host = craneLib.cargoClippy {
            inherit src;
            cargoArtifacts = hostArtifacts;
            cargoExtraArgs = "-p ubos-main-host";
            cargoClippyExtraArgs = "-- -D warnings";
            doInstallCargoArtifacts = false;
          };

          test-ubos-http = craneLib.cargoTest {
            inherit src;
            cargoArtifacts = null;
            cargoExtraArgs = "-p ubos-http";
          };

          test-ubos-api = craneLib.cargoTest {
            inherit src;
            cargoArtifacts = null;
            cargoExtraArgs = "-p ubos-api";
          };

          test-ubos-webui = craneLib.cargoTest {
            inherit src;
            cargoArtifacts = hostArtifacts;
            cargoExtraArgs = "-p ubos-webui";
            doInstallCargoArtifacts = false;
          };

          test-ubos-auth = craneLib.cargoTest {
            inherit src;
            cargoArtifacts = hostArtifacts;
            cargoExtraArgs = "-p ubos-auth";
            doInstallCargoArtifacts = false;
          };

          test-ubos-metrics = craneLib.cargoTest {
            inherit src;
            cargoArtifacts = hostArtifacts;
            cargoExtraArgs = "-p ubos-metrics";
            doInstallCargoArtifacts = false;
          };

          test-ubos-pool = craneLib.cargoTest {
            inherit src;
            cargoArtifacts = hostArtifacts;
            cargoExtraArgs = "-p ubos-pool";
            doInstallCargoArtifacts = false;
          };

          test-ubos-mining = craneLib.cargoTest {
            inherit src;
            cargoArtifacts = hostArtifacts;
            cargoExtraArgs = "-p ubos-mining";
            doInstallCargoArtifacts = false;
          };

          test-stratum-v1 = craneLib.cargoTest {
            inherit src;
            cargoArtifacts = hostArtifacts;
            cargoExtraArgs = "-p stratum-v1";
            doInstallCargoArtifacts = false;
          };

          test-mining-protocol = craneLib.cargoTest {
            inherit src;
            cargoArtifacts = hostArtifacts;
            cargoExtraArgs = "-p mining-protocol";
            doInstallCargoArtifacts = false;
          };

          test-stratum-client = craneLib.cargoTest {
            inherit src;
            cargoArtifacts = hostArtifacts;
            cargoExtraArgs = "-p stratum-client";
            doInstallCargoArtifacts = false;
          };

          clippy-ubos-thermal = craneLib.cargoClippy {
            inherit src;
            cargoArtifacts = ubosThermalArtifacts;
            cargoExtraArgs = "-p ubos-thermal";
            cargoClippyExtraArgs = "-- -D warnings";
            doInstallCargoArtifacts = false;
          };

          test-ubos-thermal = craneLib.cargoTest {
            inherit src;
            cargoArtifacts = ubosThermalArtifacts;
            cargoExtraArgs = "-p ubos-thermal";
            doInstallCargoArtifacts = false;
          };

          clippy-ubos-cooling = craneLib.cargoClippy {
            inherit src;
            cargoArtifacts = ubosCoolingArtifacts;
            cargoExtraArgs = "-p ubos-cooling";
            cargoClippyExtraArgs = "-- -D warnings";
            doInstallCargoArtifacts = false;
          };

          test-ubos-cooling = craneLib.cargoTest {
            inherit src;
            cargoArtifacts = ubosCoolingArtifacts;
            cargoExtraArgs = "-p ubos-cooling";
            doInstallCargoArtifacts = false;
          };

          clippy-ubos-manager = craneLib.cargoClippy {
            inherit src;
            cargoArtifacts = ubosManagerArtifacts;
            cargoExtraArgs = "-p ubos-manager";
            cargoClippyExtraArgs = "-- -D warnings";
            doInstallCargoArtifacts = false;
          };

          test-ubos-manager = craneLib.cargoTest {
            inherit src;
            cargoArtifacts = ubosManagerArtifacts;
            cargoExtraArgs = "-p ubos-manager";
            doInstallCargoArtifacts = false;
          };

          clippy-ubos-config = craneLib.cargoClippy {
            inherit src;
            cargoArtifacts = ubosConfigArtifacts;
            cargoExtraArgs = "-p ubos-config";
            cargoClippyExtraArgs = "-- -D warnings";
            doInstallCargoArtifacts = false;
          };

          test-ubos-config = craneLib.cargoTest {
            inherit src;
            cargoArtifacts = ubosConfigArtifacts;
            cargoExtraArgs = "-p ubos-config";
            doInstallCargoArtifacts = false;
          };

          clippy-ubos-config-std = craneLib.cargoClippy {
            inherit src;
            cargoArtifacts = ubosConfigStdArtifacts;
            cargoExtraArgs = "-p ubos-config --features std";
            cargoClippyExtraArgs = "-- -D warnings";
            doInstallCargoArtifacts = false;
          };

          test-ubos-config-std = craneLib.cargoTest {
            inherit src;
            cargoArtifacts = ubosConfigStdArtifacts;
            cargoExtraArgs = "-p ubos-config --features std";
            doInstallCargoArtifacts = false;
          };

          clippy-ubos-temperature = craneLib.cargoClippy {
            inherit src;
            cargoArtifacts = hostArtifacts;
            cargoExtraArgs = "-p ubos-temperature";
            cargoClippyExtraArgs = "-- -D warnings";
            doInstallCargoArtifacts = false;
          };

          test-ubos-temperature = craneLib.cargoTest {
            inherit src;
            cargoArtifacts = hostArtifacts;
            cargoExtraArgs = "-p ubos-temperature";
            doInstallCargoArtifacts = false;
          };

          clippy-esp32s3 = espCraneLib.cargoClippy {
            inherit src;
            cargoArtifacts = espArtifacts;
            cargoVendorDir = espCargoVendorDir;
            # Our firmware crate name collides with the esp32s3 PAC crate
            # pulled in transitively; disambiguate by pinning to @0.1.0.
            cargoExtraArgs = "-p esp32s3@0.1.0 --target xtensa-esp32s3-none-elf -Z build-std=core";
            cargoClippyExtraArgs = "-- -D warnings";
            doInstallCargoArtifacts = false;
            RUST_SRC_PATH = "${espToolchain}/lib/rustlib/src/rust/library";
            # `secp256k1-sys` is an unconditional dep via `ii-sv2` and
            # its C build script requires a cross compiler. Point `cc-rs`
            # at the espressif xtensa cross-gcc, and pass `-mlongcalls`
            # so memset/memcpy from secp256k1's C code can be reached
            # from text sections beyond the xtensa `call8` range.
            nativeBuildInputs = [ xtensaEspToolchain ];
            CC_xtensa_esp32s3_none_elf = "${xtensaEspToolchain}/bin/xtensa-esp32s3-elf-gcc";
            CFLAGS_xtensa_esp32s3_none_elf = "-mlongcalls";
          };

          build-esp32s3 = espCraneLib.cargoBuild {
            inherit src;
            cargoArtifacts = null;
            cargoVendorDir = espCargoVendorDir;
            cargoBuildCommand = "cargo build --release";
            cargoExtraArgs = "-p esp32s3@0.1.0 --target xtensa-esp32s3-none-elf -Z build-std=core";
            RUST_SRC_PATH = "${espToolchain}/lib/rustlib/src/rust/library";
            nativeBuildInputs = [ xtensaEspToolchain ];
            CC_xtensa_esp32s3_none_elf = "${xtensaEspToolchain}/bin/xtensa-esp32s3-elf-gcc";
            CFLAGS_xtensa_esp32s3_none_elf = "-mlongcalls";
            # Crane runs cargo from the workspace root, so the per-crate
            # `firmware/esp32s3/.cargo/config.toml` rustflags are out of
            # cargo's discovery path. Without `-Tlinkall.x` the linker
            # uses default section placement and secp256k1-sys's calls
            # to memset/memcpy overflow the xtensa windowed-call range.
            CARGO_TARGET_XTENSA_ESP32S3_NONE_ELF_RUSTFLAGS = "-Clink-arg=-Tlinkall.x -Clink-arg=-Tdefmt.x -Clink-arg=-nostartfiles";
          };

          clippy-esp32s3-tsens = espCraneLib.cargoClippy {
            inherit src;
            cargoArtifacts = espArtifacts;
            cargoVendorDir = espCargoVendorDir;
            cargoExtraArgs = "-p esp32s3-tsens --target xtensa-esp32s3-none-elf -Z build-std=core";
            cargoClippyExtraArgs = "-- -D warnings";
            doInstallCargoArtifacts = false;
            RUST_SRC_PATH = "${espToolchain}/lib/rustlib/src/rust/library";
          };

          clippy-load-test = craneLib.cargoClippy {
            src = loadTestSrc;
            cargoArtifacts = loadTestArtifacts;
            cargoClippyExtraArgs = "-- -D warnings";
            doInstallCargoArtifacts = false;
            strictDeps = true;
          };

          fmt = craneLib.cargoFmt {
            inherit src;
            doInstallCargoArtifacts = false;
          };

          fmt-load-test = craneLib.cargoFmt {
            src = loadTestSrc;
            doInstallCargoArtifacts = false;
          };

          test-load-test = craneLib.cargoTest {
            src = loadTestSrc;
            cargoArtifacts = loadTestArtifacts;
            strictDeps = true;
            doInstallCargoArtifacts = false;
          };

          drc-bmm-adapter =
            pkgs.runCommand "drc-bmm-adapter"
              {
                nativeBuildInputs = [ pkgs.kicad-small ];
              }
              ''
                export HOME=$(mktemp -d)
                kicad-cli pcb drc \
                  --format json \
                  --output $out \
                  ${./pcb/bmm-adapter/bmm-adapter.kicad_pcb}
              '';

          erc-bmm-adapter =
            pkgs.runCommand "erc-bmm-adapter"
              {
                nativeBuildInputs = [ pkgs.kicad-small ];
              }
              ''
                export HOME=$(mktemp -d)
                kicad-cli sch erc \
                  --severity-error \
                  --format json \
                  --output $out \
                  ${./pcb/bmm-adapter/bmm-adapter.kicad_sch}
              '';
        };

        devShells = {
          arm = pkgs.mkShell {
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
                # Cross C compiler for crates with C sources
                # (e.g. secp256k1-sys via the unconditional ii-sv2 dep).
                # Each rustc target needs a `CC_<target>` env var
                # below; cc-rs' fallback to host `gcc` does not
                # work for thumb targets.
                gcc-arm-embedded
              ]
              ++ [ deploy-rs.packages.${system}.default ];

            PROBE_RS_REMOTE_HOST = probeRsHost;
            PROBE_RS_REMOTE_TOKEN = probeRsToken;
            RUST_ANALYZER_SERVER_PATH = "${rustToolchain}/bin/rust-analyzer";

            CC_thumbv6m_none_eabi = "${pkgs.gcc-arm-embedded}/bin/arm-none-eabi-gcc";
            CC_thumbv7em_none_eabi = "${pkgs.gcc-arm-embedded}/bin/arm-none-eabi-gcc";
            CC_thumbv7em_none_eabihf = "${pkgs.gcc-arm-embedded}/bin/arm-none-eabi-gcc";
            CC_thumbv8m_main_none_eabihf = "${pkgs.gcc-arm-embedded}/bin/arm-none-eabi-gcc";

            shellHook = ''
              echo "ubos-main dev environment ready (ARM boards)"
              echo ""
              echo "  cd firmware/bmm-pico  && cargo build --release  # BMM Adapter RP2350 W5500"
              echo "  cd firmware/rp2040    && cargo build --release  # RP2040 W5500"
              echo "  cd firmware/rp2350    && cargo build --release  # RP2350 W6100"
              echo "  cd firmware/stm32f407 && cargo build --release  # STM32F407 LAN8720"
              echo "  cd firmware/stm32f439 && cargo build --release  # STM32F439 LAN8742A"
              echo ""
              echo "Inspect binary size: cargo size --release -- -A"
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
                # Version pinned for reproducibility. The same pin feeds
                # the Xtensa flake-check derivation at the top of the file.
                espup install --targets esp32s3 --toolchain-version ${espToolchainVersion} || true
                patch-esp-binaries
              fi

              # Find GCC and clang paths dynamically to avoid hardcoding version strings.
              XTENSA_GCC_PATH="$(echo "$RUSTUP_HOME"/toolchains/esp/xtensa-esp-elf/esp-*/xtensa-esp-elf/bin | head -1)"
              LIBCLANG_TOOLCHAIN_PATH="$(echo "$RUSTUP_HOME"/toolchains/esp/xtensa-esp32-elf-clang/esp-*/esp-clang/lib | head -1)"

              export PATH="$XTENSA_GCC_PATH:$RUSTUP_HOME/toolchains/esp/bin:$CARGO_HOME/bin:$PATH"

              # `cc-rs` (via secp256k1-sys, an unconditional ii-sv2 dep) needs
              # an explicit cross-compiler. `-mlongcalls` keeps memset /
              # memcpy reachable past the xtensa `call8` range.
              export CC_xtensa_esp32s3_none_elf="$XTENSA_GCC_PATH/xtensa-esp32s3-elf-gcc"
              export CFLAGS_xtensa_esp32s3_none_elf="-mlongcalls"

              if [ -d "$LIBCLANG_TOOLCHAIN_PATH" ]; then
                export LIBCLANG_PATH="$LIBCLANG_TOOLCHAIN_PATH"
              fi

              echo "ubos-main dev environment ready (ESP32-S3)"
              echo ""
              echo "  cd firmware/esp32s3 && cargo build --release  # ESP32-S3 W5100S"
              echo ""
              echo "Inspect binary size: cargo size --release -- -A"
              echo ""
              echo "For ARM boards: nix develop .#arm"
            '';
          };

          hardware = pkgs.mkShell {
            packages = [ pkgs.kicad-small ];
          };

          host = pkgs.mkShell {
            buildInputs = [ hostRustToolchain ];

            RUST_ANALYZER_SERVER_PATH = "${hostRustToolchain}/bin/rust-analyzer";

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
            ];

            CARGO_BUILD_TARGET = "arm-unknown-linux-musleabi";
            CARGO_TARGET_ARM_UNKNOWN_LINUX_MUSLEABI_LINKER = "${bmmCc}/bin/armv5tel-unknown-linux-musleabi-gcc";
            RUST_ANALYZER_SERVER_PATH = "${bmmRustToolchain}/bin/rust-analyzer";

            # Resolved at shell-entry time relative to the user's cwd, which
            # by convention is boards/ubos-main (matches the existing
            # arm/esp shells using $PWD for state directories).
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
              ++ [ deploy-rs.packages.${system}.default ];

            shellHook = ''
              echo "ubos-main — available development shells:"
              echo ""
              echo "  nix develop .#arm   — ARM boards (RP2040, RP2350, STM32F407, STM32F439)"
              echo "  nix develop .#esp   — ESP32-S3 board"
              echo "  nix develop .#host  — host-runner, native"
              echo "  nix develop .#bmm   — host-runner, BMM/miniminer cross"
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
      # redacted mesh. See nuc/nuc.nix for the host definition.
      # Deployment runs from the developer workstation, always x86_64.
      nixosConfigurations.nuc = mkNuc "x86_64-linux" [ ];

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
      packages.x86_64-linux.sd-image = sdImageFor "x86_64-linux";
      packages.aarch64-linux.sd-image = sdImageFor "aarch64-linux";
    };
}
