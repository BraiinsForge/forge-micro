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
    # probe-rs fork adding redacted redacted (RISC-V debug spec 0.11) support on
    # top of upstream 0.31.0. Fetched as a non-flake source at eval time so the
    # private gitlab.ii.zone SSH access uses the developer's git config rather
    # than the sandbox, which carries no ssh binary. mkForkProbeRsTools swaps
    # this into nixpkgs' probe-rs-tools build.
    probe-rs-redacted = {
      url = "git+ssh://git@gitlab.ii.zone/bos/ubos/probe-rs.git?ref=riscv-debug-0.11-redacted&rev=81d789213eeecac43a125f92df03e04fe669320d";
      flake = false;
    };
    # Tracks the upstream default branch (develop) now that PR#131
    # (remove-options-compat) is merged. Upstream CI populates the
    # nixos-raspberrypi cachix on every push, so the RPi kernel and
    # closure substitute instead of rebuilding under QEMU.
    nixos-raspberrypi.url = "github:nvmd/nixos-raspberrypi";
    deploy-rs.url = "github:serokell/deploy-rs";
    # ESP-IDF packaged for Nix; used only to build the esp32s3 second-stage
    # bootloader with rollback enabled. No esp-rs prebuilt rollback bootloader
    # exists, so we compile our own. It intentionally does NOT follow our
    # nixpkgs: its esp-idf derivation pins python310, which nixos-unstable has
    # dropped, so it must keep its own locked nixpkgs.
    nixpkgs-esp-dev.url = "github:mirrexagon/nixpkgs-esp-dev";
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
      nixpkgs-esp-dev,
      probe-rs-redacted,
    }@inputs:
    let
      nucHost = "redacted.redacted";
      probeRsHost = "ws://${nucHost}:8080";
      probeRsToken = "redacted";

      webuiBuildEnv = {
        # 12 chars to match the build.rs git-fallback (`rev-parse
        # --short=12`); the Nix `shortRev` default of 7 would produce
        # different footer widths inside vs. outside the sandbox.
        UBOS_WEBUI_BUILD_REV =
          let
            rev = self.rev or self.dirtyRev or null;
          in
          if rev == null then "unknown" else builtins.substring 0 12 rev;
        UBOS_WEBUI_BUILD_TIME = toString self.lastModified;
      };

      # probe-rs fork tooling: the redacted-capable fork build, the remote and
      # server patch variants, and the overlay exposing them plus the
      # probe-rs-runner wrapper. The mk* functions are reused by mkNuc below
      # to rebuild the fork against cross pkgs.
      probeRs = import ./nix/probe-rs.nix { inherit inputs; };
      inherit (probeRs)
        mkForkProbeRsTools
        mkRemoteProbeRsTools
        mkServerProbeRsTools
        ;
      probeRsOverlay = probeRs.overlay;

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
                    probe-rs-tools-remote = mkRemoteProbeRsTools (mkForkProbeRsTools aarch64Pkgs);
                    probe-rs-tools-server = mkServerProbeRsTools aarch64Pkgs (mkForkProbeRsTools aarch64Pkgs);
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
    nixpkgs.lib.recursiveUpdate
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

          # Rust toolchains and the crane instances built on them; see
          # nix/toolchains.nix for the per-universe rationale.
          toolchains = import ./nix/toolchains.nix {
            inherit
              pkgs
              nixpkgs
              system
              crane
              ;
          };
          inherit (toolchains)
            rustToolchain
            espRustToolchain
            riscvRustToolchain
            hostRustToolchain
            bmmRustToolchain
            bmmCc
            craneLib
            bmmCraneLib
            riscvCraneLib
            riscvGcc
            riscvDefmtRustflags
            ;

          # ESP32-S3 build universe: pinned Xtensa rust, cross GCC, crane
          # instance, vendored deps, the rollback-enabled bootloader, and
          # the espup binary patcher. See nix/esp.nix.
          esp = import ./nix/esp.nix {
            inherit
              pkgs
              crane
              system
              nixpkgs-esp-dev
              ;
          };
          inherit (esp)
            espToolchainVersion
            espToolchain
            espCraneLib
            espCargoVendorDir
            xtensaEspToolchain
            esp32s3Bootloader
            patchEspBinaries
            ;

          # Cleaned source trees for the crane builds. See nix/source.nix.
          sources = import ./nix/source.nix { inherit pkgs craneLib; };
          inherit (sources) src loadTestSrc;

          # Slot-size and pool-worker guard text, rendered from one
          # function so the per-board copies cannot drift apart. See
          # nix/guards.nix.
          guards = import ./nix/guards.nix { };

          # Shared dependency artifacts for the clippy, test, and build
          # checks. See nix/deps.nix for the bag-splitting rationale.
          deps = import ./nix/deps.nix {
            inherit
              pkgs
              src
              loadTestSrc
              craneLib
              bmmCraneLib
              riscvCraneLib
              riscvGcc
              riscvDefmtRustflags
              espCraneLib
              espCargoVendorDir
              espToolchain
              xtensaEspToolchain
              bmmCc
              ;
          };

          # Maximum flash bytes allowed before CI fails the size guard.
          # The value is the embassy-boot ACTIVE partition budget. The 2 MiB
          # flash is split into a 128 KiB bootloader, a 4 KiB state word, an
          # 8 KiB config store at the tail, and the remaining 1908 KiB shared
          # 50/50 between the ACTIVE and DFU slots (ACTIVE 952 KiB, DFU one
          # page larger at 956 KiB; see firmware/rp2350/memory.x). rp2350 and
          # bmm-pico reuse the rp2350-bootloader with this layout, and rp2040
          # mirrors it, so an image over 952 KiB does not fit the slot.
          # The kibibyte count is the source of truth. The ota-image-limit
          # check greps both source files for "<slotMaxKiB> * 1024" so a drift
          # in OTA_MAX_IMAGE_BYTES (ubos/ubos-api/src/system.rs) or
          # OTA_MAX_BYTES (ubos/ubos-webui/src/index.js) fails CI.
          slotMaxKiB = 952;
          slotMaxBytes = slotMaxKiB * 1024;

          # esp32s3 OTA app-slot budget. The 8 MiB flash is partitioned into
          # two equal 3 MiB app slots (ota_0/ota_1; see
          # firmware/esp32s3/partitions.csv), so the image the OTA endpoint
          # writes into the inactive slot may be up to 3 MiB — larger than the
          # RP ACTIVE budget. Keep in sync with Ota::MAX_IMAGE_BYTES in
          # firmware/esp32s3/src/config.rs.
          esp32s3SlotKiB = 3 * 1024;
          esp32s3SlotBytes = esp32s3SlotKiB * 1024;

          # Firmware version used by rp2350Image and the packages/apps outputs.
          # Pure builds default to "1" because the Nix sandbox strips all caller
          # environment variables; pass --impure to propagate UBOS_FW_VERSION.
          fwVersion =
            let
              v = builtins.getEnv "UBOS_FW_VERSION";
            in
            if v == "" then "1" else v;

          # Production firmware images, bootloader ELFs, combined BOOTSEL
          # UF2 images, and the `nix run` copy apps. See nix/images.nix
          # for the UBOS_FW_VERSION usage.
          images = import ./nix/images.nix {
            inherit
              pkgs
              src
              deps
              craneLib
              espCraneLib
              espToolchain
              espCargoVendorDir
              xtensaEspToolchain
              guards
              webuiBuildEnv
              fwVersion
              slotMaxBytes
              esp32s3SlotBytes
              ;
          };

          # Headless KiCad DRC/ERC gates for the bmm-adapter board; the
          # checks merge into `checks` below. See nix/kicad.nix.
          kicad = import ./nix/kicad.nix { inherit pkgs; };

          # redacted Logic 2 headless capture tooling. See nix/redacted.nix.
          redacted = import ./nix/redacted.nix { inherit pkgs nixpkgs system; };
          inherit (redacted) redactedPkgs redactedCapturePython redactedRun;
        in
        {
          # Per-crate gates and static guards; see nix/checks.nix for the
          # tables and generators. Each firmware crate is checked under
          # its default feature set only.
          checks =
            import ./nix/checks.nix {
              inherit
                pkgs
                src
                loadTestSrc
                deps
                craneLib
                bmmCraneLib
                riscvCraneLib
                riscvGcc
                riscvDefmtRustflags
                espCraneLib
                bmmCc
                espToolchain
                xtensaEspToolchain
                espCargoVendorDir
                esp32s3Bootloader
                guards
                webuiBuildEnv
                slotMaxKiB
                slotMaxBytes
                esp32s3SlotBytes
                ;
            }
            # The bmm-adapter DRC/ERC gates; see nix/kicad.nix.
            // kicad.checks
            # The SD image is a `packages` output, so no check covered it and
            # an evaluation regression - a nixpkgs bump whose device-tree
            # module the nixos-raspberrypi kernel cannot satisfy, say - passed
            # CI while failing `nix flake check`. Force a full NixOS evaluation
            # of the cross-built image here so that gap closes. The image
            # itself is never built: unsafeDiscardOutputDependency keeps the
            # evaluation but drops the multi-gigabyte build from the closure.
            // pkgs.lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
              sd-image-eval = pkgs.runCommand "sd-image-eval" { } ''
                echo ${builtins.unsafeDiscardOutputDependency (sdImageFor system).drvPath} > $out
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
                ++ [ deploy-rs.packages.${system}.default ];

              PROBE_RS_REMOTE_HOST = probeRsHost;
              PROBE_RS_REMOTE_TOKEN = probeRsToken;
              RUST_ANALYZER_SERVER_PATH = "${rustToolchain}/bin/rust-analyzer";

              CC_thumbv6m_none_eabi = "${pkgs.gcc-arm-embedded}/bin/arm-none-eabi-gcc";
              CC_thumbv7em_none_eabi = "${pkgs.gcc-arm-embedded}/bin/arm-none-eabi-gcc";
              CC_thumbv7em_none_eabihf = "${pkgs.gcc-arm-embedded}/bin/arm-none-eabi-gcc";
              CC_thumbv8m_main_none_eabihf = "${pkgs.gcc-arm-embedded}/bin/arm-none-eabi-gcc";

              inherit (webuiBuildEnv) UBOS_WEBUI_BUILD_REV UBOS_WEBUI_BUILD_TIME;

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
                echo "Deploy to NUC:      deploy .#redacted"
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

              PROBE_RS_REMOTE_HOST = probeRsHost;
              PROBE_RS_REMOTE_TOKEN = probeRsToken;
              RUST_ANALYZER_SERVER_PATH = "${espRustToolchain}/bin/rust-analyzer";
              LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";

              inherit (webuiBuildEnv) UBOS_WEBUI_BUILD_REV UBOS_WEBUI_BUILD_TIME;

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
                echo "  cd firmware/esp32s3 && cargo build --release  # Waveshare ESP32-S3-LCD-Driver-Board W5100S"
                echo ""
                echo "Inspect binary size: cargo size --release -- -A"
                echo ""
                echo "For ARM boards: nix develop .#arm"
              '';
            };

            # redacted redacted (dual-core RV64GC). One shell for firmware/redacted
            # (redacted 10) and the redacted-rtt lib it now uses. Both are gated in
            # `checks` (clippy-redacted/build-redacted and
            # clippy-redacted-rtt/build-redacted-rtt, built with riscvCraneLib).
            #
            # NOTE: `cargo run --release` flashes the firmware into SRAM over
            # the probe-rs redacted fork and streams the RTT log; `cargo attach`
            # reconnects to a running board. `cargo pack --release` still
            # packages the firmware into an redacted Upgrade Package (.redacted) via
            # scripts/run.sh + scripts/redacted.py (rust-objcopy from
            # cargo-binutils, python3 below), uploaded as the OTA artifact
            # through the device web UI / redacted API.
            riscv = pkgs.mkShell {
              buildInputs = with pkgs; [
                riscvRustToolchain
                # firmware/redacted links with GNU ld (`riscv64-none-elf-ld` via
                # its .cargo/config.toml rustflags) since the defmt migration:
                # rust-lld mishandles R_RISCV_PCREL_HI20 against the non-alloc
                # `.defmt` section. The check derivations already carry this
                # GCC; without it here, interactive `cargo build`/`run`/`pack`
                # fail at the link step while CI stays green.
                riscvGcc
                cargo-binutils
                cargo-bloat
                # The redacted flashes and streams its `defmt` output over RTT
                # through the probe-rs fork; the runner forwards to the NUC
                # probe-rs-server exactly as the arm/esp shells do.
                probe-rs-tools-remote
                probe-rs-runner
                pkg-config
                libudev-zero
                # ubos-webui's build.rs minifies the dashboard CSS/JS.
                esbuild
                cargo-machete
                # scripts/redacted.py builds the .redacted (SHA-256 + CRC-32) for OTA.
                python3
                # USB tooling for enumerating the attached debug probe.
                usbutils
              ];

              PROBE_RS_REMOTE_HOST = probeRsHost;
              PROBE_RS_REMOTE_TOKEN = probeRsToken;
              RUST_ANALYZER_SERVER_PATH = "${riscvRustToolchain}/bin/rust-analyzer";

              inherit (webuiBuildEnv) UBOS_WEBUI_BUILD_REV UBOS_WEBUI_BUILD_TIME;

              shellHook = ''
                echo "ubos-main dev environment ready (redacted redacted, RV64GC)"
                echo ""
                echo "  cd firmware/redacted && cargo check          # build"
                echo "  cd firmware/redacted && cargo run --release   # flash + RTT log via probe-rs (--chip redacted)"
                echo "  cd firmware/redacted && cargo attach          # stream the RTT log without reflashing"
                echo "  cd firmware/redacted && cargo pack --release  # package .redacted for OTA upload"
                echo ""
                echo "cargo run flashes over the redacted probe-rs fork; cargo pack still builds the .redacted"
                echo "(upload via web UI/API). See firmware/redacted/README.md."
                echo "For ARM boards: nix develop .#arm     ESP32-S3: nix develop .#esp"
              '';
            };

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

              inherit (webuiBuildEnv) UBOS_WEBUI_BUILD_REV UBOS_WEBUI_BUILD_TIME;

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

              inherit (webuiBuildEnv) UBOS_WEBUI_BUILD_REV UBOS_WEBUI_BUILD_TIME;

              # Resolved at shell-entry time relative to the user's cwd, which
              # by convention is the repository root (matches the existing
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

          # Production firmware images and combined BOOTSEL UF2 images;
          # see nix/images.nix for the UBOS_FW_VERSION build instructions.
          packages =
            images.packages
            // rec {
              # KiCad automation for the bmm-adapter fabrication job,
              # `nix run .#kibot`. DRC and ERC are gated separately by
              # the kicad-cli checks; kibot only generates fab outputs.
              kiauto = pkgs.callPackage ./kiauto.nix { };
              kibot = pkgs.callPackage ./kibot.nix { inherit kiauto; };

              # Headless redacted Logic 2 capture lifecycle wrapper. Also the
              # entry point of the `logic` devShell.
              redacted-run = redactedRun;

              esp32s3-bootloader = esp32s3Bootloader;
            }
            //
              # CI stage groups. Each is a linkFarm over a slice of the
              # flake's `checks`, so `nix build .#ci-<stage>` builds exactly
              # that slice and nothing else - the CI pipeline runs these and
              # a developer runs the identical command locally. No shell
              # wrapper sits between CI and nix.
              #
              # `classify` maps every check name to exactly one stage, so the
              # three groups partition `checks` with no overlap and no gaps:
              # the union of ci-format + ci-clippy + ci-check is precisely
              # `nix flake check`. ci-format is the formatting gate, ci-clippy
              # the per-crate clippy gate, and ci-check the comprehensive
              # remainder - host tests, firmware release builds, the
              # bmm-adapter DRC/ERC gates, and the static guards. A newly
              # added check that matches neither the format set nor the
              # `clippy-` prefix falls into ci-check, so coverage can never
              # silently shrink. Run a single check directly with
              # `nix build .#checks.<system>.<name>`.
              (
                let
                  checks = self.checks.${system};
                  classify =
                    name:
                    if
                      builtins.elem name [
                        "fmt"
                        "fmt-load-test"
                        "nixfmt"
                      ]
                    then
                      "ci-format"
                    else if pkgs.lib.hasPrefix "clippy-" name then
                      "ci-clippy"
                    else
                      "ci-check";
                  grouped = pkgs.lib.groupBy classify (builtins.attrNames checks);
                  farm = stage: pkgs.linkFarmFromDrvs stage (map (name: checks.${name}) (grouped.${stage} or [ ]));
                in
                {
                  ci-format = farm "ci-format";
                  ci-clippy = farm "ci-clippy";
                  ci-check = farm "ci-check";
                }
              );

          # Copy apps: `nix run .#<image>` writes the built image or UF2
          # into the repo's build/ directory. See nix/images.nix.
          apps = images.apps;
        }
      ))
      {
        # Raspberry Pi 4 support node that connects the target boards to the
        # nbvpn.braiins.cz mesh. See nuc/nuc.nix for the host definition.
        # Deployment runs from the developer workstation, always x86_64.
        nixosConfigurations.redacted = mkNuc "x86_64-linux" [ ];

        # Remote deployment via deploy-rs. Pushes the NUC NixOS closure over
        # SSH and activates it without requiring a full SD-card reflash.
        deploy.nodes.redacted = {
          hostname = nucHost;
          sshUser = "admin";
          profiles.system = {
            user = "root";
            path = deploy-rs.lib.aarch64-linux.activate.nixos self.nixosConfigurations.redacted;
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
