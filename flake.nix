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

          # Host-target load-test dependency artifacts; the crate src comes
          # from nix/source.nix.
          loadTestArtifacts = craneLib.buildDepsOnly {
            src = loadTestSrc;
            strictDeps = true;
          };
          loadTestArtifactsDev = loadTestArtifacts.overrideAttrs (_: {
            CARGO_PROFILE = "";
          });

          # Shared dependency artifacts for clippy checks. Flake checks are
          # per-crate; without reused artifacts each check recompiles the full
          # dependency graph and dominates `nix flake check` wall time.
          #
          # The bags below split host crates from firmware crates because cargo
          # unifies features within one invocation and the two groups enable
          # mutually exclusive ones such as `embassy-time/defmt` versus
          # `embassy-time/log`. Firmware bags also split per target, since
          # crane keys deps-only artifacts by `--target`.
          #
          # Host unit tests run under the dev profile so that `debug_assert!`
          # and overflow-checks fire in CI. Each release bag therefore has a
          # `-Dev` sibling built with `CARGO_PROFILE = ""`, which makes
          # crane's `cargoWithProfile` emit no `--profile` flag so cargo
          # selects its built-in test profile. Clippy and firmware builds
          # stay on the release profile and keep their original bags.
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
              "-p ubos-asic"
              "-p ubos-auth"
              "-p ubos-core"
              "-p ubos-display"
              "-p ubos-eeprom"
              "-p ubos-logging"
              "-p ubos-machine"
              "-p ubos-main-host"
              "-p ubos-metrics"
              "-p ubos-mining"
              "-p ubos-net"
              "-p ubos-psu"
              "-p ubos-snapshot"
              "-p ubos-temperature"
            ];
          };
          hostArtifactsDev = hostArtifacts.overrideAttrs (_: {
            CARGO_PROFILE = "";
          });

          hostRunnerArtifacts = craneLib.buildDepsOnly {
            inherit src;
            doCheck = false;
            cargoBuildCommand = "true";
            cargoExtraArgs = "-p host-runner";
            # host-runner depends on `ubos-webui`, whose `build.rs` shells
            # out to `esbuild` to minify the dashboard and login assets.
            # `buildDepsOnly` still executes build scripts, so esbuild
            # must be on PATH here too.
            nativeBuildInputs = [ pkgs.esbuild ];
          };
          hostRunnerArtifactsDev = hostRunnerArtifacts.overrideAttrs (_: {
            CARGO_PROFILE = "";
          });

          ubosThermalArtifacts = craneLib.buildDepsOnly {
            inherit src;
            doCheck = false;
            cargoBuildCommand = "true";
            cargoExtraArgs = "-p ubos-thermal";
          };
          ubosThermalArtifactsDev = ubosThermalArtifacts.overrideAttrs (_: {
            CARGO_PROFILE = "";
          });

          ubosConfigArtifacts = craneLib.buildDepsOnly {
            inherit src;
            doCheck = false;
            cargoBuildCommand = "true";
            cargoExtraArgs = "-p ubos-config";
          };
          ubosConfigArtifactsDev = ubosConfigArtifacts.overrideAttrs (_: {
            CARGO_PROFILE = "";
          });

          ubosConfigStdArtifacts = craneLib.buildDepsOnly {
            inherit src;
            doCheck = false;
            cargoBuildCommand = "true";
            cargoExtraArgs = "-p ubos-config --features std";
          };
          ubosConfigStdArtifactsDev = ubosConfigStdArtifacts.overrideAttrs (_: {
            CARGO_PROFILE = "";
          });

          ubosEventsArtifacts = craneLib.buildDepsOnly {
            inherit src;
            doCheck = false;
            cargoBuildCommand = "true";
            cargoExtraArgs = "-p ubos-events";
          };
          ubosEventsStdArtifacts = craneLib.buildDepsOnly {
            inherit src;
            doCheck = false;
            cargoBuildCommand = "true";
            cargoExtraArgs = "-p ubos-events --features std";
          };
          ubosEventsStdArtifactsDev = ubosEventsStdArtifacts.overrideAttrs (_: {
            CARGO_PROFILE = "";
          });

          ubosCoolingArtifacts = craneLib.buildDepsOnly {
            inherit src;
            doCheck = false;
            cargoBuildCommand = "true";
            cargoExtraArgs = "-p ubos-cooling";
          };
          ubosCoolingArtifactsDev = ubosCoolingArtifacts.overrideAttrs (_: {
            CARGO_PROFILE = "";
          });

          ubosManagerArtifacts = craneLib.buildDepsOnly {
            inherit src;
            doCheck = false;
            cargoBuildCommand = "true";
            cargoExtraArgs = "-p ubos-manager";
          };
          ubosManagerArtifactsDev = ubosManagerArtifacts.overrideAttrs (_: {
            CARGO_PROFILE = "";
          });

          ubosHttpArtifacts = craneLib.buildDepsOnly {
            inherit src;
            doCheck = false;
            cargoBuildCommand = "true";
            cargoExtraArgs = "-p ubos-http";
          };
          ubosHttpArtifactsDev = ubosHttpArtifacts.overrideAttrs (_: {
            CARGO_PROFILE = "";
          });

          ubosApiArtifacts = craneLib.buildDepsOnly {
            inherit src;
            doCheck = false;
            cargoBuildCommand = "true";
            cargoExtraArgs = "-p ubos-api";
          };
          ubosApiArtifactsDev = ubosApiArtifacts.overrideAttrs (_: {
            CARGO_PROFILE = "";
          });

          thumbv6mArtifacts = craneLib.buildDepsOnly {
            inherit src;
            doCheck = false;
            cargoBuildCommand = "cargo build --release";
            cargoExtraArgs = "-p rp2040 --target thumbv6m-none-eabi";
            nativeBuildInputs = [ pkgs.gcc-arm-embedded ];
            CC_thumbv6m_none_eabi = "${pkgs.gcc-arm-embedded}/bin/arm-none-eabi-gcc";
          };

          # Separate dep bag for the rp2040 bootloader: it uses
          # `cortex-m/critical-section-single-core` rather than
          # `embassy-rp/critical-section-impl`, and cargo's per-invocation
          # feature unification would produce duplicate `RawRestoreStateInner`
          # definitions if both crate trees were mixed into one `buildDepsOnly`.
          thumbv6mBootloaderArtifacts = craneLib.buildDepsOnly {
            inherit src;
            doCheck = false;
            cargoBuildCommand = "cargo build --release";
            cargoExtraArgs = "-p rp2040-bootloader --target thumbv6m-none-eabi";
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
            cargoBuildCommand = "cargo build --release";
            cargoExtraArgs = "-p stm32f407 --target thumbv7em-none-eabihf";
            nativeBuildInputs = [ pkgs.gcc-arm-embedded ];
            CC_thumbv7em_none_eabihf = "${pkgs.gcc-arm-embedded}/bin/arm-none-eabi-gcc";
          };

          stm32f439Artifacts = craneLib.buildDepsOnly {
            inherit src;
            doCheck = false;
            cargoBuildCommand = "cargo build --release";
            cargoExtraArgs = "-p stm32f439 --target thumbv7em-none-eabihf";
            nativeBuildInputs = [ pkgs.gcc-arm-embedded ];
            CC_thumbv7em_none_eabihf = "${pkgs.gcc-arm-embedded}/bin/arm-none-eabi-gcc";
          };

          thumbv8mArtifacts = craneLib.buildDepsOnly {
            inherit src;
            doCheck = false;
            cargoBuildCommand = "cargo build --release";
            cargoExtraArgs = builtins.concatStringsSep " " [
              "-p bmm-pico"
              "-p rp2350"
              "-p rp235x-sha256"
              "--target thumbv8m.main-none-eabihf"
            ];
            nativeBuildInputs = [ pkgs.gcc-arm-embedded ];
            CC_thumbv8m_main_none_eabihf = "${pkgs.gcc-arm-embedded}/bin/arm-none-eabi-gcc";
          };

          # hashnode cannot share the thumbv8m bag: it selects
          # embassy-rp/rp235xb while the bag's crates select rp235xa, so
          # the bag's embassy-rp would never match and every consumer of
          # a unified bag would rebuild it.
          hashnodeArtifacts = craneLib.buildDepsOnly {
            inherit src;
            doCheck = false;
            cargoBuildCommand = "cargo build --release";
            cargoExtraArgs = "-p hashnode --target thumbv8m.main-none-eabihf";
            nativeBuildInputs = [ pkgs.gcc-arm-embedded ];
            CC_thumbv8m_main_none_eabihf = "${pkgs.gcc-arm-embedded}/bin/arm-none-eabi-gcc";
          };

          ubosOtaArtifacts = craneLib.buildDepsOnly {
            inherit src;
            doCheck = false;
            cargoBuildCommand = "true";
            cargoExtraArgs = "-p ubos-ota --target thumbv8m.main-none-eabihf --features rp,embassy-rp/rp235xa,embassy-rp/imagedef-none";
            CC_thumbv8m_main_none_eabihf = "${pkgs.gcc-arm-embedded}/bin/arm-none-eabi-gcc";
          };

          # ubos-firmware references the eight subsystem crates, which unify
          # firmware-only embassy features; it is gated on one ARM target
          # rather than the host bag for the same reason the firmware crates
          # are. No host test: the crate is all `const`.
          ubosFirmwareArtifacts = craneLib.buildDepsOnly {
            inherit src;
            doCheck = false;
            cargoBuildCommand = "cargo build --release";
            cargoExtraArgs = "-p ubos-firmware --target thumbv8m.main-none-eabihf";
            nativeBuildInputs = [ pkgs.gcc-arm-embedded ];
            CC_thumbv8m_main_none_eabihf = "${pkgs.gcc-arm-embedded}/bin/arm-none-eabi-gcc";
          };

          # Separate dep bag for the bootloader: it uses
          # `cortex-m/critical-section-single-core` rather than
          # `embassy-rp/critical-section-impl`, and cargo's per-invocation
          # feature unification would produce duplicate `RawRestoreStateInner`
          # definitions if both crate trees are mixed in one `buildDepsOnly`.
          thumbv8mBootloaderArtifacts = craneLib.buildDepsOnly {
            inherit src;
            doCheck = false;
            cargoBuildCommand = "cargo build --release";
            cargoExtraArgs = "-p rp2350-bootloader --target thumbv8m.main-none-eabihf";
            nativeBuildInputs = [ pkgs.gcc-arm-embedded ];
            CC_thumbv8m_main_none_eabihf = "${pkgs.gcc-arm-embedded}/bin/arm-none-eabi-gcc";
          };

          # redacted redacted (RV64GC) dependency closure for the clippy-redacted check.
          # No C deps, so no cross GCC needed for `cc`; esbuild is for
          # ubos-webui's build.rs. `riscvGcc` is still a build input, purely
          # for its `riscv64-none-elf-ld` binary (see riscvDefmtRustflags).
          # `riscvDefmtRustflags` matches the consuming check so the cached
          # deps layer is reused (crane keys artifacts by build env).
          riscvredactedArtifacts = riscvCraneLib.buildDepsOnly {
            inherit src;
            doCheck = false;
            cargoBuildCommand = "true";
            cargoExtraArgs = "-p redacted --target riscv64gc-unknown-none-elf";
            nativeBuildInputs = [
              pkgs.esbuild
              riscvGcc
            ];
            CARGO_TARGET_RISCV64GC_UNKNOWN_NONE_ELF_RUSTFLAGS = riscvDefmtRustflags;
          };

          # redacted-rtt is a standalone no_std lib: no C deps, no build.rs, so
          # neither esbuild nor a cross GCC is needed. `log` and `defmt` are
          # mutually exclusive backends (each defines its own CHAN_NAME), so
          # unlike a normal `--all-features` closure, the two are covered by
          # separate artifact sets below rather than one `--all-features` build.
          riscvredactedRttArtifacts = riscvCraneLib.buildDepsOnly {
            inherit src;
            doCheck = false;
            cargoBuildCommand = "true";
            cargoExtraArgs = "-p redacted-rtt --target riscv64gc-unknown-none-elf";
            CARGO_TARGET_RISCV64GC_UNKNOWN_NONE_ELF_RUSTFLAGS = "-C panic=abort";
          };

          # `defmt`-backend closure: used by firmware/redacted and firmware/redacted.
          riscvredactedRttDefmtArtifacts = riscvCraneLib.buildDepsOnly {
            inherit src;
            doCheck = false;
            cargoBuildCommand = "true";
            cargoExtraArgs = "-p redacted-rtt --target riscv64gc-unknown-none-elf --no-default-features --features defmt";
            CARGO_TARGET_RISCV64GC_UNKNOWN_NONE_ELF_RUSTFLAGS = "-C panic=abort";
          };

          bmmArtifacts = bmmCraneLib.buildDepsOnly {
            inherit src;
            doCheck = false;
            cargoBuildCommand = "true";
            cargoExtraArgs = "-p host-runner --target arm-unknown-linux-musleabi";
            # `pkgs.esbuild` is needed for ubos-webui's build.rs, which
            # runs as part of host-runner's dependency closure.
            nativeBuildInputs = [
              bmmCc
              pkgs.esbuild
            ];
            CARGO_TARGET_ARM_UNKNOWN_LINUX_MUSLEABI_LINKER = "${bmmCc}/bin/armv5tel-unknown-linux-musleabi-gcc";
            CC_arm_unknown_linux_musleabi = "${bmmCc}/bin/armv5tel-unknown-linux-musleabi-gcc";
          };

          # The esp deps bag stays check-only. crane's buildDepsOnly stubs the
          # esp32s3 bin with a dummy `main`, and a real `cargo build` fails to
          # link that stub under `-Z build-std=core`: the esp crt0 references
          # `main` while the crate is `#![no_main]`. A `cargo check` never
          # links, so it builds. The esp release builds below therefore keep
          # `cargoArtifacts = null` and rebuild their dependency closure; the
          # other targets' bags do codegen and the builds reuse them.
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

          # Production rp2350 firmware image derivation. Shared by
          # packages.rp2350-image (nix build) and apps.rp2350-image (nix run).
          rp2350Image = craneLib.cargoBuild {
            inherit src;
            cargoArtifacts = thumbv8mArtifacts;
            cargoBuildCommand = "cargo build --release";
            cargoExtraArgs = "-p rp2350 --target thumbv8m.main-none-eabihf";
            nativeBuildInputs = [
              pkgs.gcc-arm-embedded
              pkgs.esbuild
            ];
            CC_thumbv8m_main_none_eabihf = "${pkgs.gcc-arm-embedded}/bin/arm-none-eabi-gcc";
            inherit (webuiBuildEnv) UBOS_WEBUI_BUILD_REV UBOS_WEBUI_BUILD_TIME;
            UBOS_FW_VERSION = fwVersion;
            # Do not copy the cargo target directory into $out; we only
            # want the raw binary image.
            doInstallCargoArtifacts = false;
            # Slot-size and pool-worker guards match build-rp2350 in
            # checks; the difference is that the bin is kept in $out
            # instead of being deleted.
            installPhaseCommand = ''
              mkdir -p "$out"
              arm-none-eabi-objcopy -O binary \
                target/thumbv8m.main-none-eabihf/release/rp2350 \
                "$out/rp2350-v${fwVersion}.bin"
              size=$(wc -c < "$out/rp2350-v${fwVersion}.bin")
              echo "rp2350 image: $size bytes / ${toString slotMaxBytes} bytes budget, version ${fwVersion}"
              if [ "$size" -gt "${toString slotMaxBytes}" ]; then
                echo "ERROR: rp2350 release image ($size bytes) exceeds the" \
                     "${toString slotMaxBytes}-byte slot limit. See docs/OTA.md."
                exit 1
              fi
              if [ -n "''${POOL_USER:-}" ]; then
                echo "ERROR: POOL_USER is set; a shipped image must not bake a pool worker."
                exit 1
              fi
              if grep -qa "braiinstest" "$out/rp2350-v${fwVersion}.bin"; then
                echo "ERROR: rp2350 image contains the test pool worker 'braiinstest'; it must not ship."
                exit 1
              fi
            '';
          };

          # Production bmm-pico firmware image derivation. Shared by
          # packages.bmm-pico-image (nix build) and apps.bmm-pico-image
          # (nix run). The bmm-pico crate bakes POOL_USER via its
          # .cargo/config.toml, but cargo only reads that config when run
          # from the crate directory; this crane build runs from the flake
          # root, so POOL_USER stays unset and the test worker is not baked.
          bmmPicoImage = craneLib.cargoBuild {
            inherit src;
            cargoArtifacts = thumbv8mArtifacts;
            cargoBuildCommand = "cargo build --release";
            cargoExtraArgs = "-p bmm-pico --target thumbv8m.main-none-eabihf";
            nativeBuildInputs = [
              pkgs.gcc-arm-embedded
              pkgs.esbuild
            ];
            CC_thumbv8m_main_none_eabihf = "${pkgs.gcc-arm-embedded}/bin/arm-none-eabi-gcc";
            inherit (webuiBuildEnv) UBOS_WEBUI_BUILD_REV UBOS_WEBUI_BUILD_TIME;
            UBOS_FW_VERSION = fwVersion;
            # Do not copy the cargo target directory into $out; we only
            # want the raw binary image.
            doInstallCargoArtifacts = false;
            # Slot-size and pool-worker guards match build-bmm-pico in
            # checks; the difference is that the bin is kept in $out
            # instead of being deleted.
            installPhaseCommand = ''
              mkdir -p "$out"
              arm-none-eabi-objcopy -O binary \
                target/thumbv8m.main-none-eabihf/release/bmm-pico \
                "$out/bmm-pico-v${fwVersion}.bin"
              size=$(wc -c < "$out/bmm-pico-v${fwVersion}.bin")
              echo "bmm-pico image: $size bytes / ${toString slotMaxBytes} bytes budget, version ${fwVersion}"
              if [ "$size" -gt "${toString slotMaxBytes}" ]; then
                echo "ERROR: bmm-pico release image ($size bytes) exceeds the" \
                     "${toString slotMaxBytes}-byte slot limit. See docs/OTA.md."
                exit 1
              fi
              if [ -n "''${POOL_USER:-}" ]; then
                echo "ERROR: POOL_USER is set; a shipped image must not bake a pool worker."
                exit 1
              fi
              if grep -qa "braiinstest" "$out/bmm-pico-v${fwVersion}.bin"; then
                echo "ERROR: bmm-pico image contains the test pool worker 'braiinstest'; it must not ship."
                exit 1
              fi
            '';
          };

          # Production rp2040 firmware image derivation. Shared by
          # packages.rp2040-image (nix build) and apps.rp2040-image (nix run).
          rp2040Image = craneLib.cargoBuild {
            inherit src;
            cargoArtifacts = thumbv6mArtifacts;
            cargoBuildCommand = "cargo build --release";
            cargoExtraArgs = "-p rp2040 --target thumbv6m-none-eabi";
            nativeBuildInputs = [
              pkgs.gcc-arm-embedded
              pkgs.esbuild
            ];
            CC_thumbv6m_none_eabi = "${pkgs.gcc-arm-embedded}/bin/arm-none-eabi-gcc";
            inherit (webuiBuildEnv) UBOS_WEBUI_BUILD_REV UBOS_WEBUI_BUILD_TIME;
            UBOS_FW_VERSION = fwVersion;
            # Do not copy the cargo target directory into $out; we only
            # want the raw binary image.
            doInstallCargoArtifacts = false;
            # Slot-size and pool-worker guards match build-rp2040 in
            # checks; the difference is that the bin is kept in $out
            # instead of being deleted.
            installPhaseCommand = ''
              mkdir -p "$out"
              arm-none-eabi-objcopy -O binary \
                target/thumbv6m-none-eabi/release/rp2040 \
                "$out/rp2040-v${fwVersion}.bin"
              size=$(wc -c < "$out/rp2040-v${fwVersion}.bin")
              echo "rp2040 image: $size bytes / ${toString slotMaxBytes} bytes budget, version ${fwVersion}"
              if [ "$size" -gt "${toString slotMaxBytes}" ]; then
                echo "ERROR: rp2040 release image ($size bytes) exceeds the" \
                     "${toString slotMaxBytes}-byte slot limit. See docs/OTA.md."
                exit 1
              fi
              if [ -n "''${POOL_USER:-}" ]; then
                echo "ERROR: POOL_USER is set; a shipped image must not bake a pool worker."
                exit 1
              fi
              if grep -qa "braiinstest" "$out/rp2040-v${fwVersion}.bin"; then
                echo "ERROR: rp2040 image contains the test pool worker 'braiinstest'; it must not ship."
                exit 1
              fi
            '';
          };

          # Production hashnode firmware image derivation. Shared by
          # packages.hashnode-image (nix build) and apps.hashnode-uf2
          # (nix run).
          hashnodeImage = craneLib.cargoBuild {
            inherit src;
            cargoArtifacts = hashnodeArtifacts;
            cargoBuildCommand = "cargo build --release";
            cargoExtraArgs = "-p hashnode --target thumbv8m.main-none-eabihf";
            nativeBuildInputs = [
              pkgs.gcc-arm-embedded
              pkgs.esbuild
            ];
            CC_thumbv8m_main_none_eabihf = "${pkgs.gcc-arm-embedded}/bin/arm-none-eabi-gcc";
            inherit (webuiBuildEnv) UBOS_WEBUI_BUILD_REV UBOS_WEBUI_BUILD_TIME;
            UBOS_FW_VERSION = fwVersion;
            # Do not copy the cargo target directory into $out; we only
            # want the raw binary image.
            doInstallCargoArtifacts = false;
            # The hashnode .cargo/config.toml bakes a test POOL_USER for
            # dev flashing, but crane builds from the flake root where
            # cargo does not read it; the guard proves it stayed out.
            installPhaseCommand = ''
              mkdir -p "$out"
              arm-none-eabi-objcopy -O binary \
                target/thumbv8m.main-none-eabihf/release/hashnode \
                "$out/hashnode-v${fwVersion}.bin"
              size=$(wc -c < "$out/hashnode-v${fwVersion}.bin")
              echo "hashnode image: $size bytes / ${toString slotMaxBytes} bytes budget, version ${fwVersion}"
              if [ "$size" -gt "${toString slotMaxBytes}" ]; then
                echo "ERROR: hashnode release image ($size bytes) exceeds the" \
                     "${toString slotMaxBytes}-byte slot limit. See docs/OTA.md."
                exit 1
              fi
              if [ -n "''${POOL_USER:-}" ]; then
                echo "ERROR: POOL_USER is set; a shipped image must not bake a pool worker."
                exit 1
              fi
              if grep -qa "braiinstest" "$out/hashnode-v${fwVersion}.bin"; then
                echo "ERROR: hashnode image contains the test pool worker 'braiinstest'; it must not ship."
                exit 1
              fi
            '';
          };

          # Bootloader ELF for the rp2350, bmm-pico, and hashnode boards.
          # All three use the rp2350-bootloader crate; it occupies flash
          # offset 0x10000000 and carries the image-def the bootrom
          # validates. The crate is built with the rp235xa feature but
          # touches no GPIO, so the binary also runs on the hashnode's
          # RP2350B. The combined UF2 derivations bundle this ELF with the
          # app binary so the image is drag-droppable via the BOOTSEL
          # mass-storage interface.
          rp2350Bootloader = craneLib.cargoBuild {
            inherit src;
            cargoArtifacts = thumbv8mBootloaderArtifacts;
            cargoBuildCommand = "cargo build --release";
            cargoExtraArgs = "-p rp2350-bootloader --target thumbv8m.main-none-eabihf";
            nativeBuildInputs = [ pkgs.gcc-arm-embedded ];
            CC_thumbv8m_main_none_eabihf = "${pkgs.gcc-arm-embedded}/bin/arm-none-eabi-gcc";
            doInstallCargoArtifacts = false;
            installPhaseCommand = ''
              mkdir -p "$out"
              cp target/thumbv8m.main-none-eabihf/release/rp2350-bootloader "$out/boot.elf"
            '';
          };

          # Bootloader ELF for the rp2040 board. It occupies flash offset
          # 0x10000000 and carries the image-def the bootrom validates. The
          # combined UF2 derivation bundles this ELF with the app binary so the
          # image is drag-droppable via the BOOTSEL mass-storage interface.
          rp2040Bootloader = craneLib.cargoBuild {
            inherit src;
            cargoArtifacts = thumbv6mBootloaderArtifacts;
            cargoBuildCommand = "cargo build --release";
            cargoExtraArgs = "-p rp2040-bootloader --target thumbv6m-none-eabi";
            nativeBuildInputs = [ pkgs.gcc-arm-embedded ];
            CC_thumbv6m_none_eabi = "${pkgs.gcc-arm-embedded}/bin/arm-none-eabi-gcc";
            doInstallCargoArtifacts = false;
            installPhaseCommand = ''
              mkdir -p "$out"
              cp target/thumbv6m-none-eabi/release/rp2040-bootloader "$out/boot.elf"
            '';
          };

          # Build a combined BOOTSEL UF2 image from a raw app binary and a
          # bootloader ELF. The bootloader covers offset 0x10000000 (the
          # offset-0 region the bootrom validates); the app binary is placed at
          # 0x10021000 (the ACTIVE partition). UF2 blocks carry individual
          # flash addresses, so concatenating the two single-region UF2 files
          # yields one drag-drop image spanning both flash regions.
          mkCombinedUf2 =
            {
              name,
              appBin,
              bootElf,
              family,
            }:
            pkgs.runCommand "${name}-uf2" { nativeBuildInputs = [ pkgs.picotool ]; } ''
              mkdir -p "$out"
              out_uf2="$out/${name}-v${fwVersion}.uf2"
              picotool uf2 convert ${bootElf}/boot.elf boot.uf2
              picotool uf2 convert ${appBin} -t bin app.uf2 -o 0x10021000 --family ${family}
              cat boot.uf2 app.uf2 > "$out_uf2"
              echo "${name} combined UF2 (bootloader + app v${fwVersion}):"
              picotool info "$out_uf2" || true
            '';

          rp2350Uf2 = mkCombinedUf2 {
            name = "rp2350";
            appBin = "${rp2350Image}/rp2350-v${fwVersion}.bin";
            bootElf = rp2350Bootloader;
            family = "rp2350-arm-s";
          };

          bmmPicoUf2 = mkCombinedUf2 {
            name = "bmm-pico";
            appBin = "${bmmPicoImage}/bmm-pico-v${fwVersion}.bin";
            bootElf = rp2350Bootloader;
            family = "rp2350-arm-s";
          };

          rp2040Uf2 = mkCombinedUf2 {
            name = "rp2040";
            appBin = "${rp2040Image}/rp2040-v${fwVersion}.bin";
            bootElf = rp2040Bootloader;
            family = "rp2040";
          };

          hashnodeUf2 = mkCombinedUf2 {
            name = "hashnode";
            appBin = "${hashnodeImage}/hashnode-v${fwVersion}.bin";
            bootElf = rp2350Bootloader;
            family = "rp2350-arm-s";
          };

          # Headless KiCad DRC/ERC gates for the bmm-adapter board; the
          # checks merge into `checks` below. See nix/kicad.nix.
          kicad = import ./nix/kicad.nix { inherit pkgs; };

          # redacted Logic 2 headless capture tooling. See nix/redacted.nix.
          redacted = import ./nix/redacted.nix { inherit pkgs nixpkgs system; };
          inherit (redacted) redactedPkgs redactedCapturePython redactedRun;

          # Production esp32s3 firmware image. Shared by packages.esp32s3-image
          # (nix build) and apps.esp32s3-image (nix run). espflash converts the
          # linked ELF into the esp-idf app-image format the OTA endpoint writes
          # into an ota_N slot. The size guard uses the 3 MiB app-slot budget
          # (esp32s3SlotBytes), which is what the REST handler enforces on the
          # upload for this board.
          esp32s3Image = espCraneLib.cargoBuild {
            inherit src;
            cargoArtifacts = null;
            cargoVendorDir = espCargoVendorDir;
            cargoBuildCommand = "cargo build --release";
            cargoExtraArgs = "-p esp32s3@0.1.0 --target xtensa-esp32s3-none-elf -Z build-std=core";
            CARGO_TARGET_XTENSA_ESP32S3_NONE_ELF_RUSTFLAGS = "-Clink-arg=-Tlinkall.x -Clink-arg=-Tdefmt.x -Clink-arg=-nostartfiles";
            RUST_SRC_PATH = "${espToolchain}/lib/rustlib/src/rust/library";
            nativeBuildInputs = [
              xtensaEspToolchain
              pkgs.esbuild
              pkgs.espflash
            ];
            CC_xtensa_esp32s3_none_elf = "${xtensaEspToolchain}/bin/xtensa-esp32s3-elf-gcc";
            CFLAGS_xtensa_esp32s3_none_elf = "-mlongcalls";
            inherit (webuiBuildEnv) UBOS_WEBUI_BUILD_REV UBOS_WEBUI_BUILD_TIME;
            UBOS_FW_VERSION = fwVersion;
            doInstallCargoArtifacts = false;
            installPhaseCommand = ''
              mkdir -p "$out"
              # espflash reads/writes a user cache dir; keep it in the sandbox.
              export HOME="$TMPDIR"
              espflash save-image --chip esp32s3 --flash-size 8mb \
                target/xtensa-esp32s3-none-elf/release/esp32s3 \
                "$out/esp32s3-v${fwVersion}.bin"
              size=$(wc -c < "$out/esp32s3-v${fwVersion}.bin")
              echo "esp32s3 image: $size bytes / ${toString esp32s3SlotBytes} bytes OTA app-slot budget, version ${fwVersion}"
              if [ "$size" -gt "${toString esp32s3SlotBytes}" ]; then
                echo "ERROR: esp32s3 release image ($size bytes) exceeds the" \
                     "${toString esp32s3SlotBytes}-byte OTA app slot. See docs/OTA.md."
                exit 1
              fi
              if [ -n "''${POOL_USER:-}" ]; then
                echo "ERROR: POOL_USER is set; a shipped image must not bake a pool worker."
                exit 1
              fi
              if grep -qa "braiinstest" "$out/esp32s3-v${fwVersion}.bin"; then
                echo "ERROR: esp32s3 image contains the test pool worker 'braiinstest'; it must not ship."
                exit 1
              fi
            '';
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
          checks = rec {
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
              nativeBuildInputs = [
                pkgs.gcc-arm-embedded
                pkgs.esbuild
              ];
              CC_thumbv8m_main_none_eabihf = "${pkgs.gcc-arm-embedded}/bin/arm-none-eabi-gcc";
              inherit (webuiBuildEnv) UBOS_WEBUI_BUILD_REV UBOS_WEBUI_BUILD_TIME;
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
              cargoArtifacts = thumbv8mArtifacts;
              doInstallCargoArtifacts = false;
              cargoBuildCommand = "cargo build --release";
              cargoExtraArgs = "-p bmm-pico --target thumbv8m.main-none-eabihf";
              nativeBuildInputs = [
                pkgs.gcc-arm-embedded
                pkgs.esbuild
              ];
              CC_thumbv8m_main_none_eabihf = "${pkgs.gcc-arm-embedded}/bin/arm-none-eabi-gcc";
              inherit (webuiBuildEnv) UBOS_WEBUI_BUILD_REV UBOS_WEBUI_BUILD_TIME;
              # Slot-size guard plus pool-worker guard. See docs/OTA.md
              # for the flash layout and slot budget.
              postBuild = ''
                elf=target/thumbv8m.main-none-eabihf/release/bmm-pico
                bin=$(mktemp --suffix=.bin)
                arm-none-eabi-objcopy -O binary "$elf" "$bin"
                size=$(wc -c < "$bin")
                echo "bmm-pico slot-size check: $size bytes / ${toString slotMaxBytes} bytes budget"
                if [ "$size" -gt "${toString slotMaxBytes}" ]; then
                  echo "ERROR: bmm-pico release image ($size bytes) exceeds the" \
                       "${toString slotMaxBytes}-byte slot limit. See docs/OTA.md."
                  rm -f "$bin"; exit 1
                fi
                # No pool worker may be baked into a shipped image. POOL_USER
                # overrides the empty default at build time; a developer uses
                # POOL_USER=braiinstest to test mining. A pure nix build strips
                # POOL_USER from the sandbox, so the default empty worker is
                # always used and a worker cannot leak; these two guards only
                # bite on an `--impure` build that forwarded POOL_USER, where
                # they fail the build before such an artifact escapes.
                if [ -n "''${POOL_USER:-}" ]; then
                  echo "ERROR: POOL_USER is set; a shipped image must not bake a pool worker."
                  rm -f "$bin"; exit 1
                fi
                if grep -qa "braiinstest" "$bin"; then
                  echo "ERROR: bmm-pico image contains the test pool worker 'braiinstest'; it must not ship."
                  rm -f "$bin"; exit 1
                fi
                rm -f "$bin"
              '';
            };

            build-bmm-pico-no-display = craneLib.cargoBuild {
              inherit src;
              cargoArtifacts = thumbv8mArtifacts;
              doInstallCargoArtifacts = false;
              cargoBuildCommand = "cargo build --release";
              cargoExtraArgs = "-p bmm-pico --target thumbv8m.main-none-eabihf --no-default-features";
              nativeBuildInputs = [
                pkgs.gcc-arm-embedded
                pkgs.esbuild
              ];
              CC_thumbv8m_main_none_eabihf = "${pkgs.gcc-arm-embedded}/bin/arm-none-eabi-gcc";
              inherit (webuiBuildEnv) UBOS_WEBUI_BUILD_REV UBOS_WEBUI_BUILD_TIME;
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
              nativeBuildInputs = [
                pkgs.gcc-arm-embedded
                pkgs.esbuild
              ];
              CC_thumbv6m_none_eabi = "${pkgs.gcc-arm-embedded}/bin/arm-none-eabi-gcc";
              inherit (webuiBuildEnv) UBOS_WEBUI_BUILD_REV UBOS_WEBUI_BUILD_TIME;
            };

            build-rp2040 = craneLib.cargoBuild {
              inherit src;
              cargoArtifacts = thumbv6mArtifacts;
              doInstallCargoArtifacts = false;
              cargoBuildCommand = "cargo build --release";
              cargoExtraArgs = "-p rp2040 --target thumbv6m-none-eabi";
              nativeBuildInputs = [
                pkgs.gcc-arm-embedded
                pkgs.esbuild
              ];
              CC_thumbv6m_none_eabi = "${pkgs.gcc-arm-embedded}/bin/arm-none-eabi-gcc";
              inherit (webuiBuildEnv) UBOS_WEBUI_BUILD_REV UBOS_WEBUI_BUILD_TIME;
              postBuild = ''
                elf=target/thumbv6m-none-eabi/release/rp2040
                bin=$(mktemp --suffix=.bin)
                arm-none-eabi-objcopy -O binary "$elf" "$bin"
                size=$(wc -c < "$bin")
                echo "rp2040 slot-size check: $size bytes / ${toString slotMaxBytes} bytes budget"
                if [ "$size" -gt "${toString slotMaxBytes}" ]; then
                  echo "ERROR: rp2040 release image ($size bytes) exceeds the" \
                       "${toString slotMaxBytes}-byte slot limit. See docs/OTA.md."
                  rm -f "$bin"; exit 1
                fi
                # No pool worker may be baked into a shipped image. POOL_USER
                # overrides the empty default at build time; a developer uses
                # POOL_USER=braiinstest to test mining. A pure nix build strips
                # POOL_USER from the sandbox, so the default empty worker is
                # always used and a worker cannot leak; these two guards only
                # bite on an `--impure` build that forwarded POOL_USER, where
                # they fail the build before such an artifact escapes.
                if [ -n "''${POOL_USER:-}" ]; then
                  echo "ERROR: POOL_USER is set; a shipped image must not bake a pool worker."
                  rm -f "$bin"; exit 1
                fi
                if grep -qa "braiinstest" "$bin"; then
                  echo "ERROR: rp2040 image contains the test pool worker 'braiinstest'; it must not ship."
                  rm -f "$bin"; exit 1
                fi
                rm -f "$bin"
              '';
            };

            build-rp2040-no-display = craneLib.cargoBuild {
              inherit src;
              cargoArtifacts = thumbv6mArtifacts;
              doInstallCargoArtifacts = false;
              cargoBuildCommand = "cargo build --release";
              cargoExtraArgs = "-p rp2040 --target thumbv6m-none-eabi --no-default-features";
              nativeBuildInputs = [
                pkgs.gcc-arm-embedded
                pkgs.esbuild
              ];
              CC_thumbv6m_none_eabi = "${pkgs.gcc-arm-embedded}/bin/arm-none-eabi-gcc";
              inherit (webuiBuildEnv) UBOS_WEBUI_BUILD_REV UBOS_WEBUI_BUILD_TIME;
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
              nativeBuildInputs = [
                pkgs.gcc-arm-embedded
                pkgs.esbuild
              ];
              CC_thumbv8m_main_none_eabihf = "${pkgs.gcc-arm-embedded}/bin/arm-none-eabi-gcc";
              inherit (webuiBuildEnv) UBOS_WEBUI_BUILD_REV UBOS_WEBUI_BUILD_TIME;
            };

            build-rp2350 = craneLib.cargoBuild {
              inherit src;
              cargoArtifacts = thumbv8mArtifacts;
              doInstallCargoArtifacts = false;
              cargoBuildCommand = "cargo build --release";
              cargoExtraArgs = "-p rp2350 --target thumbv8m.main-none-eabihf";
              nativeBuildInputs = [
                pkgs.gcc-arm-embedded
                pkgs.esbuild
              ];
              CC_thumbv8m_main_none_eabihf = "${pkgs.gcc-arm-embedded}/bin/arm-none-eabi-gcc";
              inherit (webuiBuildEnv) UBOS_WEBUI_BUILD_REV UBOS_WEBUI_BUILD_TIME;
              # Slot-size guard plus pool-worker guard. See docs/OTA.md
              # for the flash layout and slot budget.
              postBuild = ''
                elf=target/thumbv8m.main-none-eabihf/release/rp2350
                bin=$(mktemp --suffix=.bin)
                arm-none-eabi-objcopy -O binary "$elf" "$bin"
                size=$(wc -c < "$bin")
                echo "rp2350 slot-size check: $size bytes / ${toString slotMaxBytes} bytes budget"
                if [ "$size" -gt "${toString slotMaxBytes}" ]; then
                  echo "ERROR: rp2350 release image ($size bytes) exceeds the" \
                       "${toString slotMaxBytes}-byte slot limit. See docs/OTA.md."
                  rm -f "$bin"; exit 1
                fi
                # No pool worker may be baked into a shipped image. POOL_USER
                # overrides the empty default at build time; a developer uses
                # POOL_USER=braiinstest to test mining. A pure nix build strips
                # POOL_USER from the sandbox, so the default empty worker is
                # always used and a worker cannot leak; these two guards only
                # bite on an `--impure` build that forwarded POOL_USER, where
                # they fail the build before such an artifact escapes.
                if [ -n "''${POOL_USER:-}" ]; then
                  echo "ERROR: POOL_USER is set; a shipped image must not bake a pool worker."
                  rm -f "$bin"; exit 1
                fi
                if grep -qa "braiinstest" "$bin"; then
                  echo "ERROR: rp2350 image contains the test pool worker 'braiinstest'; it must not ship."
                  rm -f "$bin"; exit 1
                fi
                rm -f "$bin"
              '';
            };

            build-rp2350-no-display = craneLib.cargoBuild {
              inherit src;
              cargoArtifacts = thumbv8mArtifacts;
              doInstallCargoArtifacts = false;
              cargoBuildCommand = "cargo build --release";
              cargoExtraArgs = "-p rp2350 --target thumbv8m.main-none-eabihf --no-default-features";
              nativeBuildInputs = [
                pkgs.gcc-arm-embedded
                pkgs.esbuild
              ];
              CC_thumbv8m_main_none_eabihf = "${pkgs.gcc-arm-embedded}/bin/arm-none-eabi-gcc";
              inherit (webuiBuildEnv) UBOS_WEBUI_BUILD_REV UBOS_WEBUI_BUILD_TIME;
            };

            clippy-hashnode = craneLib.cargoClippy {
              inherit src;
              cargoArtifacts = hashnodeArtifacts;
              cargoExtraArgs = "-p hashnode --target thumbv8m.main-none-eabihf";
              cargoClippyExtraArgs = "-- -D warnings";
              doInstallCargoArtifacts = false;
              nativeBuildInputs = [
                pkgs.gcc-arm-embedded
                pkgs.esbuild
              ];
              CC_thumbv8m_main_none_eabihf = "${pkgs.gcc-arm-embedded}/bin/arm-none-eabi-gcc";
              inherit (webuiBuildEnv) UBOS_WEBUI_BUILD_REV UBOS_WEBUI_BUILD_TIME;
            };

            build-hashnode = craneLib.cargoBuild {
              inherit src;
              cargoArtifacts = hashnodeArtifacts;
              cargoBuildCommand = "cargo build --release";
              cargoExtraArgs = "-p hashnode --target thumbv8m.main-none-eabihf";
              nativeBuildInputs = [
                pkgs.gcc-arm-embedded
                pkgs.esbuild
              ];
              CC_thumbv8m_main_none_eabihf = "${pkgs.gcc-arm-embedded}/bin/arm-none-eabi-gcc";
              inherit (webuiBuildEnv) UBOS_WEBUI_BUILD_REV UBOS_WEBUI_BUILD_TIME;
            };

            clippy-rp2350-bootloader = craneLib.cargoClippy {
              inherit src;
              cargoArtifacts = thumbv8mBootloaderArtifacts;
              cargoExtraArgs = "-p rp2350-bootloader --target thumbv8m.main-none-eabihf";
              cargoClippyExtraArgs = "-- -D warnings";
              doInstallCargoArtifacts = false;
              nativeBuildInputs = [ pkgs.gcc-arm-embedded ];
              CC_thumbv8m_main_none_eabihf = "${pkgs.gcc-arm-embedded}/bin/arm-none-eabi-gcc";
            };

            build-rp2350-bootloader = craneLib.cargoBuild {
              inherit src;
              cargoArtifacts = thumbv8mBootloaderArtifacts;
              doInstallCargoArtifacts = false;
              cargoBuildCommand = "cargo build --release";
              cargoExtraArgs = "-p rp2350-bootloader --target thumbv8m.main-none-eabihf";
              nativeBuildInputs = [ pkgs.gcc-arm-embedded ];
              CC_thumbv8m_main_none_eabihf = "${pkgs.gcc-arm-embedded}/bin/arm-none-eabi-gcc";
            };

            clippy-rp2040-bootloader = craneLib.cargoClippy {
              inherit src;
              cargoArtifacts = thumbv6mBootloaderArtifacts;
              cargoExtraArgs = "-p rp2040-bootloader --target thumbv6m-none-eabi";
              cargoClippyExtraArgs = "-- -D warnings";
              doInstallCargoArtifacts = false;
              nativeBuildInputs = [ pkgs.gcc-arm-embedded ];
              CC_thumbv6m_none_eabi = "${pkgs.gcc-arm-embedded}/bin/arm-none-eabi-gcc";
            };

            build-rp2040-bootloader = craneLib.cargoBuild {
              inherit src;
              cargoArtifacts = thumbv6mBootloaderArtifacts;
              doInstallCargoArtifacts = false;
              cargoBuildCommand = "cargo build --release";
              cargoExtraArgs = "-p rp2040-bootloader --target thumbv6m-none-eabi";
              nativeBuildInputs = [ pkgs.gcc-arm-embedded ];
              CC_thumbv6m_none_eabi = "${pkgs.gcc-arm-embedded}/bin/arm-none-eabi-gcc";
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
              nativeBuildInputs = [
                pkgs.gcc-arm-embedded
                pkgs.esbuild
              ];
              CC_thumbv7em_none_eabihf = "${pkgs.gcc-arm-embedded}/bin/arm-none-eabi-gcc";
              inherit (webuiBuildEnv) UBOS_WEBUI_BUILD_REV UBOS_WEBUI_BUILD_TIME;
            };

            build-stm32f407 = craneLib.cargoBuild {
              inherit src;
              cargoArtifacts = stm32f407Artifacts;
              doInstallCargoArtifacts = false;
              cargoBuildCommand = "cargo build --release";
              cargoExtraArgs = "-p stm32f407 --target thumbv7em-none-eabihf";
              nativeBuildInputs = [
                pkgs.gcc-arm-embedded
                pkgs.esbuild
              ];
              CC_thumbv7em_none_eabihf = "${pkgs.gcc-arm-embedded}/bin/arm-none-eabi-gcc";
              inherit (webuiBuildEnv) UBOS_WEBUI_BUILD_REV UBOS_WEBUI_BUILD_TIME;
              # Pool-worker guard. See the bmm-pico build for the full
              # rationale. This board is not an OTA target and has no slot
              # budget, so the guard greps the ELF directly rather than
              # objcopying a slot-sized binary.
              postBuild = ''
                elf=target/thumbv7em-none-eabihf/release/stm32f407
                if [ -n "''${POOL_USER:-}" ]; then
                  echo "ERROR: POOL_USER is set; a shipped image must not bake a pool worker."
                  exit 1
                fi
                if grep -qa "braiinstest" "$elf"; then
                  echo "ERROR: stm32f407 image contains the test pool worker 'braiinstest'; it must not ship."
                  exit 1
                fi
              '';
            };

            build-stm32f407-no-display = craneLib.cargoBuild {
              inherit src;
              cargoArtifacts = stm32f407Artifacts;
              doInstallCargoArtifacts = false;
              cargoBuildCommand = "cargo build --release";
              cargoExtraArgs = "-p stm32f407 --target thumbv7em-none-eabihf --no-default-features";
              nativeBuildInputs = [
                pkgs.gcc-arm-embedded
                pkgs.esbuild
              ];
              CC_thumbv7em_none_eabihf = "${pkgs.gcc-arm-embedded}/bin/arm-none-eabi-gcc";
              inherit (webuiBuildEnv) UBOS_WEBUI_BUILD_REV UBOS_WEBUI_BUILD_TIME;
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
              nativeBuildInputs = [
                pkgs.gcc-arm-embedded
                pkgs.esbuild
              ];
              CC_thumbv7em_none_eabihf = "${pkgs.gcc-arm-embedded}/bin/arm-none-eabi-gcc";
              inherit (webuiBuildEnv) UBOS_WEBUI_BUILD_REV UBOS_WEBUI_BUILD_TIME;
            };

            build-stm32f439 = craneLib.cargoBuild {
              inherit src;
              cargoArtifacts = stm32f439Artifacts;
              doInstallCargoArtifacts = false;
              cargoBuildCommand = "cargo build --release";
              cargoExtraArgs = "-p stm32f439 --target thumbv7em-none-eabihf";
              nativeBuildInputs = [
                pkgs.gcc-arm-embedded
                pkgs.esbuild
              ];
              CC_thumbv7em_none_eabihf = "${pkgs.gcc-arm-embedded}/bin/arm-none-eabi-gcc";
              inherit (webuiBuildEnv) UBOS_WEBUI_BUILD_REV UBOS_WEBUI_BUILD_TIME;
              # Pool-worker guard. See the bmm-pico build for the full
              # rationale. This board is not an OTA target and has no slot
              # budget, so the guard greps the ELF directly rather than
              # objcopying a slot-sized binary.
              postBuild = ''
                elf=target/thumbv7em-none-eabihf/release/stm32f439
                if [ -n "''${POOL_USER:-}" ]; then
                  echo "ERROR: POOL_USER is set; a shipped image must not bake a pool worker."
                  exit 1
                fi
                if grep -qa "braiinstest" "$elf"; then
                  echo "ERROR: stm32f439 image contains the test pool worker 'braiinstest'; it must not ship."
                  exit 1
                fi
              '';
            };

            build-stm32f439-no-display = craneLib.cargoBuild {
              inherit src;
              cargoArtifacts = stm32f439Artifacts;
              doInstallCargoArtifacts = false;
              cargoBuildCommand = "cargo build --release";
              cargoExtraArgs = "-p stm32f439 --target thumbv7em-none-eabihf --no-default-features";
              nativeBuildInputs = [
                pkgs.gcc-arm-embedded
                pkgs.esbuild
              ];
              CC_thumbv7em_none_eabihf = "${pkgs.gcc-arm-embedded}/bin/arm-none-eabi-gcc";
              inherit (webuiBuildEnv) UBOS_WEBUI_BUILD_REV UBOS_WEBUI_BUILD_TIME;
            };

            clippy-ubos-core = craneLib.cargoClippy {
              inherit src;
              cargoArtifacts = hostArtifacts;
              cargoExtraArgs = "-p ubos-core";
              cargoClippyExtraArgs = "--all-targets -- -D warnings";
              doInstallCargoArtifacts = false;
            };

            test-ubos-core = craneLib.cargoTest {
              inherit src;
              cargoArtifacts = hostArtifactsDev;
              CARGO_PROFILE = "";
              cargoExtraArgs = "-p ubos-core";
              doInstallCargoArtifacts = false;
            };

            clippy-ubos-http = craneLib.cargoClippy {
              inherit src;
              cargoArtifacts = hostArtifacts;
              cargoExtraArgs = "-p ubos-http";
              cargoClippyExtraArgs = "--all-targets -- -D warnings";
              doInstallCargoArtifacts = false;
            };

            clippy-ubos-api = craneLib.cargoClippy {
              inherit src;
              cargoArtifacts = hostArtifacts;
              cargoExtraArgs = "-p ubos-api";
              cargoClippyExtraArgs = "--all-targets -- -D warnings";
              doInstallCargoArtifacts = false;
            };

            clippy-ubos-ota = craneLib.cargoClippy {
              inherit src;
              cargoArtifacts = ubosOtaArtifacts;
              cargoExtraArgs = "-p ubos-ota --target thumbv8m.main-none-eabihf --features rp,embassy-rp/rp235xa,embassy-rp/imagedef-none";
              cargoClippyExtraArgs = "-- -D warnings";
              doInstallCargoArtifacts = false;
              CC_thumbv8m_main_none_eabihf = "${pkgs.gcc-arm-embedded}/bin/arm-none-eabi-gcc";
            };

            build-ubos-firmware = craneLib.cargoBuild {
              inherit src;
              cargoArtifacts = ubosFirmwareArtifacts;
              cargoBuildCommand = "cargo build --release";
              cargoExtraArgs = "-p ubos-firmware --target thumbv8m.main-none-eabihf";
              doInstallCargoArtifacts = false;
              nativeBuildInputs = [ pkgs.gcc-arm-embedded ];
              CC_thumbv8m_main_none_eabihf = "${pkgs.gcc-arm-embedded}/bin/arm-none-eabi-gcc";
            };

            clippy-ubos-firmware = craneLib.cargoClippy {
              inherit src;
              cargoArtifacts = ubosFirmwareArtifacts;
              cargoExtraArgs = "-p ubos-firmware --target thumbv8m.main-none-eabihf";
              cargoClippyExtraArgs = "-- -D warnings";
              doInstallCargoArtifacts = false;
              nativeBuildInputs = [ pkgs.gcc-arm-embedded ];
              CC_thumbv8m_main_none_eabihf = "${pkgs.gcc-arm-embedded}/bin/arm-none-eabi-gcc";
            };

            clippy-ubos-auth = craneLib.cargoClippy {
              inherit src;
              cargoArtifacts = hostArtifacts;
              cargoExtraArgs = "-p ubos-auth";
              cargoClippyExtraArgs = "--all-targets -- -D warnings";
              doInstallCargoArtifacts = false;
            };

            clippy-ubos-metrics = craneLib.cargoClippy {
              inherit src;
              cargoArtifacts = hostArtifacts;
              cargoExtraArgs = "-p ubos-metrics";
              cargoClippyExtraArgs = "--all-targets -- -D warnings";
              doInstallCargoArtifacts = false;
            };

            clippy-ubos-webui = craneLib.cargoClippy {
              inherit src;
              cargoArtifacts = hostArtifacts;
              cargoExtraArgs = "-p ubos-webui";
              cargoClippyExtraArgs = "--all-targets -- -D warnings";
              doInstallCargoArtifacts = false;
              # build.rs runs `esbuild --minify` on the dashboard CSS/JS
              # before gzipping into OUT_DIR.
              nativeBuildInputs = [ pkgs.esbuild ];
              inherit (webuiBuildEnv) UBOS_WEBUI_BUILD_REV UBOS_WEBUI_BUILD_TIME;
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
              cargoClippyExtraArgs = "--all-targets -- -D warnings";
              doInstallCargoArtifacts = false;
            };

            test-ubos-mdns = craneLib.cargoTest {
              inherit src;
              cargoArtifacts = hostArtifactsDev;
              CARGO_PROFILE = "";
              cargoExtraArgs = "-p ubos-mdns";
              doInstallCargoArtifacts = false;
            };

            clippy-ubos-net = craneLib.cargoClippy {
              inherit src;
              cargoArtifacts = hostArtifacts;
              cargoExtraArgs = "-p ubos-net";
              cargoClippyExtraArgs = "--all-targets -- -D warnings";
              doInstallCargoArtifacts = false;
            };

            test-ubos-net = craneLib.cargoTest {
              inherit src;
              cargoArtifacts = hostArtifactsDev;
              CARGO_PROFILE = "";
              cargoExtraArgs = "-p ubos-net";
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
              cargoClippyExtraArgs = "--all-targets -- -D warnings";
              doInstallCargoArtifacts = false;
              # `pkgs.esbuild` is needed for ubos-webui's build.rs, which
              # runs as part of host-runner's dependency closure.
              nativeBuildInputs = [ pkgs.esbuild ];
              inherit (webuiBuildEnv) UBOS_WEBUI_BUILD_REV UBOS_WEBUI_BUILD_TIME;
            };

            test-host-runner = craneLib.cargoTest {
              inherit src;
              cargoArtifacts = hostRunnerArtifactsDev;
              CARGO_PROFILE = "";
              cargoExtraArgs = "-p host-runner";
              doInstallCargoArtifacts = false;
              # `pkgs.esbuild` is needed for ubos-webui's build.rs, which
              # runs as part of host-runner's dependency closure.
              nativeBuildInputs = [ pkgs.esbuild ];
              inherit (webuiBuildEnv) UBOS_WEBUI_BUILD_REV UBOS_WEBUI_BUILD_TIME;
            };

            clippy-host-runner-bmm = bmmCraneLib.cargoClippy {
              inherit src;
              cargoArtifacts = bmmArtifacts;
              cargoExtraArgs = "-p host-runner --target arm-unknown-linux-musleabi";
              cargoClippyExtraArgs = "-- -D warnings";
              doInstallCargoArtifacts = false;
              # `pkgs.esbuild` is needed for ubos-webui's build.rs, which
              # runs as part of host-runner's dependency closure.
              nativeBuildInputs = [
                bmmCc
                pkgs.esbuild
              ];
              CARGO_TARGET_ARM_UNKNOWN_LINUX_MUSLEABI_LINKER = "${bmmCc}/bin/armv5tel-unknown-linux-musleabi-gcc";
              # SV2 pulls in `secp256k1-sys`, whose C build script defers to
              # `cc-rs`. Point it at the same musleabi cross-gcc the linker
              # uses so the C objects share the target ABI.
              CC_arm_unknown_linux_musleabi = "${bmmCc}/bin/armv5tel-unknown-linux-musleabi-gcc";
              inherit (webuiBuildEnv) UBOS_WEBUI_BUILD_REV UBOS_WEBUI_BUILD_TIME;
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
              cargoClippyExtraArgs = "--all-targets -- -D warnings";
              doInstallCargoArtifacts = false;
            };

            test-ubos-psu = craneLib.cargoTest {
              inherit src;
              cargoArtifacts = hostArtifactsDev;
              CARGO_PROFILE = "";
              cargoExtraArgs = "-p ubos-psu";
              doInstallCargoArtifacts = false;
            };

            clippy-ubos-eeprom = craneLib.cargoClippy {
              inherit src;
              cargoArtifacts = hostArtifacts;
              cargoExtraArgs = "-p ubos-eeprom";
              cargoClippyExtraArgs = "--all-targets -- -D warnings";
              doInstallCargoArtifacts = false;
            };

            test-ubos-eeprom = craneLib.cargoTest {
              inherit src;
              cargoArtifacts = hostArtifactsDev;
              CARGO_PROFILE = "";
              cargoExtraArgs = "-p ubos-eeprom";
              doInstallCargoArtifacts = false;
            };

            clippy-embedded-i2c-unix = craneLib.cargoClippy {
              inherit src;
              cargoArtifacts = hostArtifacts;
              cargoExtraArgs = "-p embedded-i2c-unix";
              cargoClippyExtraArgs = "--all-targets -- -D warnings";
              doInstallCargoArtifacts = false;
            };

            test-embedded-i2c-unix = craneLib.cargoTest {
              inherit src;
              cargoArtifacts = hostArtifactsDev;
              CARGO_PROFILE = "";
              cargoExtraArgs = "-p embedded-i2c-unix";
              doInstallCargoArtifacts = false;
            };

            clippy-ubos-mining = craneLib.cargoClippy {
              inherit src;
              cargoArtifacts = hostArtifacts;
              cargoExtraArgs = "-p ubos-mining";
              cargoClippyExtraArgs = "--all-targets -- -D warnings";
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
              cargoClippyExtraArgs = "--all-targets -- -D warnings";
              doInstallCargoArtifacts = false;
            };

            clippy-ubos-asic = craneLib.cargoClippy {
              inherit src;
              cargoArtifacts = hostArtifacts;
              cargoExtraArgs = "-p ubos-asic";
              cargoClippyExtraArgs = "--all-targets -- -D warnings";
              doInstallCargoArtifacts = false;
            };

            clippy-ubos-machine = craneLib.cargoClippy {
              inherit src;
              cargoArtifacts = hostArtifacts;
              cargoExtraArgs = "-p ubos-machine";
              cargoClippyExtraArgs = "-- -D warnings";
              doInstallCargoArtifacts = false;
            };

            test-ubos-machine = craneLib.cargoTest {
              inherit src;
              cargoArtifacts = hostArtifactsDev;
              CARGO_PROFILE = "";
              cargoExtraArgs = "-p ubos-machine";
              doInstallCargoArtifacts = false;
            };

            clippy-stratum-v1 = craneLib.cargoClippy {
              inherit src;
              cargoArtifacts = hostArtifacts;
              cargoExtraArgs = "-p stratum-v1";
              cargoClippyExtraArgs = "--all-targets -- -D warnings";
              doInstallCargoArtifacts = false;
            };

            clippy-stratum-v2 = craneLib.cargoClippy {
              inherit src;
              cargoArtifacts = hostArtifacts;
              cargoExtraArgs = "-p stratum-v2";
              cargoClippyExtraArgs = "--all-targets -- -D warnings";
              doInstallCargoArtifacts = false;
            };

            clippy-stratum-client = craneLib.cargoClippy {
              inherit src;
              cargoArtifacts = hostArtifacts;
              cargoExtraArgs = "-p stratum-client";
              cargoClippyExtraArgs = "--all-targets -- -D warnings";
              doInstallCargoArtifacts = false;
            };

            clippy-ubos-main-host = craneLib.cargoClippy {
              inherit src;
              cargoArtifacts = hostArtifacts;
              cargoExtraArgs = "-p ubos-main-host";
              cargoClippyExtraArgs = "-- -D warnings";
              doInstallCargoArtifacts = false;
            };

            clippy-ubos-snapshot = craneLib.cargoClippy {
              inherit src;
              cargoArtifacts = hostArtifacts;
              cargoExtraArgs = "-p ubos-snapshot";
              cargoClippyExtraArgs = "--all-targets -- -D warnings";
              doInstallCargoArtifacts = false;
            };

            test-ubos-snapshot = craneLib.cargoTest {
              inherit src;
              cargoArtifacts = hostArtifactsDev;
              CARGO_PROFILE = "";
              cargoExtraArgs = "-p ubos-snapshot";
              doInstallCargoArtifacts = false;
            };

            test-ubos-http = craneLib.cargoTest {
              inherit src;
              cargoArtifacts = ubosHttpArtifactsDev;
              CARGO_PROFILE = "";
              cargoExtraArgs = "-p ubos-http";
            };

            # Host tests build with defmt off. ubos-api's default `defmt`
            # feature routes its `ubos_logging` calls through `defmt::*`
            # macros, which reference `_defmt_acquire`/`_defmt_timestamp` and
            # friends. A host test binary links no `defmt::global_logger`, so
            # those symbols are undefined and the test fails to link.
            test-ubos-api = craneLib.cargoTest {
              inherit src;
              cargoArtifacts = ubosApiArtifactsDev;
              CARGO_PROFILE = "";
              cargoExtraArgs = "-p ubos-api --no-default-features";
            };

            test-ubos-webui = craneLib.cargoTest {
              inherit src;
              cargoArtifacts = hostArtifactsDev;
              CARGO_PROFILE = "";
              cargoExtraArgs = "-p ubos-webui";
              doInstallCargoArtifacts = false;
              # build.rs runs `esbuild --minify` on the dashboard CSS/JS
              # before gzipping into OUT_DIR.
              nativeBuildInputs = [ pkgs.esbuild ];
              inherit (webuiBuildEnv) UBOS_WEBUI_BUILD_REV UBOS_WEBUI_BUILD_TIME;
            };

            test-ubos-auth = craneLib.cargoTest {
              inherit src;
              cargoArtifacts = hostArtifactsDev;
              CARGO_PROFILE = "";
              cargoExtraArgs = "-p ubos-auth";
              doInstallCargoArtifacts = false;
            };

            test-ubos-metrics = craneLib.cargoTest {
              inherit src;
              cargoArtifacts = hostArtifactsDev;
              CARGO_PROFILE = "";
              cargoExtraArgs = "-p ubos-metrics";
              doInstallCargoArtifacts = false;
            };

            test-ubos-mining = craneLib.cargoTest {
              inherit src;
              cargoArtifacts = hostArtifactsDev;
              CARGO_PROFILE = "";
              cargoExtraArgs = "-p ubos-mining";
              doInstallCargoArtifacts = false;
            };

            test-stratum-v1 = craneLib.cargoTest {
              inherit src;
              cargoArtifacts = hostArtifactsDev;
              CARGO_PROFILE = "";
              cargoExtraArgs = "-p stratum-v1";
              doInstallCargoArtifacts = false;
            };

            test-mining-protocol = craneLib.cargoTest {
              inherit src;
              cargoArtifacts = hostArtifactsDev;
              CARGO_PROFILE = "";
              cargoExtraArgs = "-p mining-protocol";
              doInstallCargoArtifacts = false;
            };

            test-ubos-asic = craneLib.cargoTest {
              inherit src;
              cargoArtifacts = hostArtifactsDev;
              CARGO_PROFILE = "";
              cargoExtraArgs = "-p ubos-asic";
              doInstallCargoArtifacts = false;
            };

            test-stratum-client = craneLib.cargoTest {
              inherit src;
              cargoArtifacts = hostArtifactsDev;
              CARGO_PROFILE = "";
              cargoExtraArgs = "-p stratum-client";
              doInstallCargoArtifacts = false;
            };

            test-stratum-v2 = craneLib.cargoTest {
              inherit src;
              cargoArtifacts = hostArtifactsDev;
              CARGO_PROFILE = "";
              cargoExtraArgs = "-p stratum-v2";
              doInstallCargoArtifacts = false;
            };

            clippy-ubos-thermal = craneLib.cargoClippy {
              inherit src;
              cargoArtifacts = ubosThermalArtifacts;
              cargoExtraArgs = "-p ubos-thermal";
              cargoClippyExtraArgs = "--all-targets -- -D warnings";
              doInstallCargoArtifacts = false;
            };

            test-ubos-thermal = craneLib.cargoTest {
              inherit src;
              cargoArtifacts = ubosThermalArtifactsDev;
              CARGO_PROFILE = "";
              cargoExtraArgs = "-p ubos-thermal";
              doInstallCargoArtifacts = false;
            };

            clippy-ubos-cooling = craneLib.cargoClippy {
              inherit src;
              cargoArtifacts = ubosCoolingArtifacts;
              cargoExtraArgs = "-p ubos-cooling";
              cargoClippyExtraArgs = "--all-targets -- -D warnings";
              doInstallCargoArtifacts = false;
            };

            test-ubos-cooling = craneLib.cargoTest {
              inherit src;
              cargoArtifacts = ubosCoolingArtifactsDev;
              CARGO_PROFILE = "";
              cargoExtraArgs = "-p ubos-cooling";
              doInstallCargoArtifacts = false;
            };

            clippy-ubos-manager = craneLib.cargoClippy {
              inherit src;
              cargoArtifacts = ubosManagerArtifacts;
              cargoExtraArgs = "-p ubos-manager";
              cargoClippyExtraArgs = "--all-targets -- -D warnings";
              doInstallCargoArtifacts = false;
            };

            test-ubos-manager = craneLib.cargoTest {
              inherit src;
              cargoArtifacts = ubosManagerArtifactsDev;
              CARGO_PROFILE = "";
              cargoExtraArgs = "-p ubos-manager";
              doInstallCargoArtifacts = false;
            };

            clippy-ubos-config = craneLib.cargoClippy {
              inherit src;
              cargoArtifacts = ubosConfigArtifacts;
              cargoExtraArgs = "-p ubos-config";
              cargoClippyExtraArgs = "--all-targets -- -D warnings";
              doInstallCargoArtifacts = false;
            };

            test-ubos-config = craneLib.cargoTest {
              inherit src;
              cargoArtifacts = ubosConfigArtifactsDev;
              CARGO_PROFILE = "";
              cargoExtraArgs = "-p ubos-config";
              doInstallCargoArtifacts = false;
            };

            clippy-ubos-events = craneLib.cargoClippy {
              inherit src;
              cargoArtifacts = ubosEventsArtifacts;
              cargoExtraArgs = "-p ubos-events";
              cargoClippyExtraArgs = "-- -D warnings";
              doInstallCargoArtifacts = false;
            };

            clippy-ubos-events-std = craneLib.cargoClippy {
              inherit src;
              cargoArtifacts = ubosEventsStdArtifacts;
              cargoExtraArgs = "-p ubos-events --features std";
              cargoClippyExtraArgs = "--all-targets -- -D warnings";
              doInstallCargoArtifacts = false;
            };

            test-ubos-events = craneLib.cargoTest {
              inherit src;
              cargoArtifacts = ubosEventsStdArtifactsDev;
              CARGO_PROFILE = "";
              cargoExtraArgs = "-p ubos-events --features std";
              doInstallCargoArtifacts = false;
            };

            clippy-ubos-config-std = craneLib.cargoClippy {
              inherit src;
              cargoArtifacts = ubosConfigStdArtifacts;
              cargoExtraArgs = "-p ubos-config --features std";
              cargoClippyExtraArgs = "--all-targets -- -D warnings";
              doInstallCargoArtifacts = false;
            };

            test-ubos-config-std = craneLib.cargoTest {
              inherit src;
              cargoArtifacts = ubosConfigStdArtifactsDev;
              CARGO_PROFILE = "";
              cargoExtraArgs = "-p ubos-config --features std";
              doInstallCargoArtifacts = false;
            };

            clippy-ubos-temperature = craneLib.cargoClippy {
              inherit src;
              cargoArtifacts = hostArtifacts;
              cargoExtraArgs = "-p ubos-temperature";
              cargoClippyExtraArgs = "--all-targets -- -D warnings";
              doInstallCargoArtifacts = false;
            };

            test-ubos-temperature = craneLib.cargoTest {
              inherit src;
              cargoArtifacts = hostArtifactsDev;
              CARGO_PROFILE = "";
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
              nativeBuildInputs = [
                xtensaEspToolchain
                pkgs.esbuild
              ];
              CC_xtensa_esp32s3_none_elf = "${xtensaEspToolchain}/bin/xtensa-esp32s3-elf-gcc";
              CFLAGS_xtensa_esp32s3_none_elf = "-mlongcalls";
              inherit (webuiBuildEnv) UBOS_WEBUI_BUILD_REV UBOS_WEBUI_BUILD_TIME;
            };

            build-esp32s3 = espCraneLib.cargoBuild {
              inherit src;
              # No codegen bag to reuse; see espArtifacts above.
              cargoArtifacts = null;
              cargoVendorDir = espCargoVendorDir;
              cargoBuildCommand = "cargo build --release";
              cargoExtraArgs = "-p esp32s3@0.1.0 --target xtensa-esp32s3-none-elf -Z build-std=core";
              RUST_SRC_PATH = "${espToolchain}/lib/rustlib/src/rust/library";
              nativeBuildInputs = [
                xtensaEspToolchain
                pkgs.esbuild
                pkgs.espflash
              ];
              CC_xtensa_esp32s3_none_elf = "${xtensaEspToolchain}/bin/xtensa-esp32s3-elf-gcc";
              CFLAGS_xtensa_esp32s3_none_elf = "-mlongcalls";
              # Crane runs cargo from the workspace root, so the per-crate
              # `firmware/esp32s3/.cargo/config.toml` rustflags are out of
              # cargo's discovery path. Without `-Tlinkall.x` the linker
              # uses default section placement and secp256k1-sys's calls
              # to memset/memcpy overflow the xtensa windowed-call range.
              CARGO_TARGET_XTENSA_ESP32S3_NONE_ELF_RUSTFLAGS = "-Clink-arg=-Tlinkall.x -Clink-arg=-Tdefmt.x -Clink-arg=-nostartfiles";
              inherit (webuiBuildEnv) UBOS_WEBUI_BUILD_REV UBOS_WEBUI_BUILD_TIME;
              # Slot-size and pool-worker guards. The esp32s3 is an OTA target
              # with a 3 MiB app-slot budget, so this runs espflash
              # save-image to produce the esp-idf app image the OTA endpoint
              # writes into a slot and checks its size, mirroring the rp board
              # checks. The size guard must live here, not only in
              # packages.esp32s3-image: `nix flake check` builds checks.* but
              # merely evaluates packages.*, so a guard in the image package
              # alone would never gate an oversized image in CI.
              postBuild = ''
                elf=target/xtensa-esp32s3-none-elf/release/esp32s3
                bin=$(mktemp --suffix=.bin)
                # espflash reads/writes a user cache dir; keep it in the sandbox.
                export HOME="$TMPDIR"
                espflash save-image --chip esp32s3 --flash-size 8mb "$elf" "$bin"
                size=$(wc -c < "$bin")
                echo "esp32s3 slot-size check: $size bytes / ${toString esp32s3SlotBytes} bytes OTA app-slot budget"
                if [ "$size" -gt "${toString esp32s3SlotBytes}" ]; then
                  echo "ERROR: esp32s3 release image ($size bytes) exceeds the" \
                       "${toString esp32s3SlotBytes}-byte OTA app slot. See docs/OTA.md."
                  rm -f "$bin"; exit 1
                fi
                if [ -n "''${POOL_USER:-}" ]; then
                  echo "ERROR: POOL_USER is set; a shipped image must not bake a pool worker."
                  rm -f "$bin"; exit 1
                fi
                if grep -qa "braiinstest" "$bin"; then
                  echo "ERROR: esp32s3 image contains the test pool worker 'braiinstest'; it must not ship."
                  rm -f "$bin"; exit 1
                fi
                rm -f "$bin"
              '';
            };

            build-esp32s3-no-display = espCraneLib.cargoBuild {
              inherit src;
              # No codegen bag to reuse; see espArtifacts above.
              cargoArtifacts = null;
              cargoVendorDir = espCargoVendorDir;
              cargoBuildCommand = "cargo build --release";
              cargoExtraArgs = "-p esp32s3@0.1.0 --target xtensa-esp32s3-none-elf -Z build-std=core --no-default-features";
              RUST_SRC_PATH = "${espToolchain}/lib/rustlib/src/rust/library";
              nativeBuildInputs = [
                xtensaEspToolchain
                pkgs.esbuild
              ];
              CC_xtensa_esp32s3_none_elf = "${xtensaEspToolchain}/bin/xtensa-esp32s3-elf-gcc";
              CFLAGS_xtensa_esp32s3_none_elf = "-mlongcalls";
              # Crane runs cargo from the workspace root, so the per-crate
              # `firmware/esp32s3/.cargo/config.toml` rustflags are out of
              # cargo's discovery path. Without `-Tlinkall.x` the linker
              # uses default section placement and secp256k1-sys's calls
              # to memset/memcpy overflow the xtensa windowed-call range.
              CARGO_TARGET_XTENSA_ESP32S3_NONE_ELF_RUSTFLAGS = "-Clink-arg=-Tlinkall.x -Clink-arg=-Tdefmt.x -Clink-arg=-nostartfiles";
              inherit (webuiBuildEnv) UBOS_WEBUI_BUILD_REV UBOS_WEBUI_BUILD_TIME;
            };

            # Gate the OTA bootloader so a broken sdkconfig fails CI. This
            # pulls the esp-idf closure on first build; it is cached until the
            # nixpkgs-esp-dev input or the stub project changes.
            build-esp32s3-bootloader = esp32s3Bootloader;

            # Lint the ubos-ota esp backend. clippy only lints the top crate,
            # so the esp32s3 firmware build compiles this code but does not
            # lint it; this check does. The chip features are supplied on the
            # CLI, matching clippy-ubos-ota for the rp backend.
            clippy-ubos-ota-esp = espCraneLib.cargoClippy {
              inherit src;
              cargoArtifacts = espArtifacts;
              cargoVendorDir = espCargoVendorDir;
              cargoExtraArgs =
                "-p ubos-ota --target xtensa-esp32s3-none-elf -Z build-std=core "
                + "--features esp,esp-hal/esp32s3,esp-hal/unstable,esp-storage/esp32s3,"
                + "esp-storage/critical-section,esp-bootloader-esp-idf/esp32s3";
              cargoClippyExtraArgs = "-- -D warnings";
              doInstallCargoArtifacts = false;
              RUST_SRC_PATH = "${espToolchain}/lib/rustlib/src/rust/library";
              nativeBuildInputs = [
                xtensaEspToolchain
                pkgs.esbuild
              ];
              CC_xtensa_esp32s3_none_elf = "${xtensaEspToolchain}/bin/xtensa-esp32s3-elf-gcc";
              CFLAGS_xtensa_esp32s3_none_elf = "-mlongcalls";
              inherit (webuiBuildEnv) UBOS_WEBUI_BUILD_REV UBOS_WEBUI_BUILD_TIME;
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

            # redacted redacted (RV64GC). Mirrors the rp2350 board checks: a tier-2
            # target with prebuilt rust-std, so no `-Z build-std`. No C deps, so
            # no cross GCC for `cc`; esbuild is only for ubos-webui's build.rs.
            # `riscvDefmtRustflags` is passed here because crane builds from
            # the workspace root and ignores the crate's .cargo/config.toml,
            # where the same flags live (panic strategy, and the linker
            # override defmt's `.defmt` section needs — see that file and
            # riscvDefmtRustflags above). riscvGcc is a build input purely for
            # the `riscv64-none-elf-ld` binary that override resolves to.
            clippy-redacted = riscvCraneLib.cargoClippy {
              inherit src;
              cargoArtifacts = riscvredactedArtifacts;
              cargoExtraArgs = "-p redacted --target riscv64gc-unknown-none-elf";
              cargoClippyExtraArgs = "-- -D warnings";
              doInstallCargoArtifacts = false;
              nativeBuildInputs = [
                pkgs.esbuild
                riscvGcc
              ];
              CARGO_TARGET_RISCV64GC_UNKNOWN_NONE_ELF_RUSTFLAGS = riscvDefmtRustflags;
              inherit (webuiBuildEnv) UBOS_WEBUI_BUILD_REV UBOS_WEBUI_BUILD_TIME;
            };

            build-redacted = riscvCraneLib.cargoBuild {
              inherit src;
              cargoArtifacts = null;
              cargoBuildCommand = "cargo build --release";
              cargoExtraArgs = "-p redacted --target riscv64gc-unknown-none-elf";
              nativeBuildInputs = [
                pkgs.esbuild
                riscvGcc
              ];
              CARGO_TARGET_RISCV64GC_UNKNOWN_NONE_ELF_RUSTFLAGS = riscvDefmtRustflags;
              inherit (webuiBuildEnv) UBOS_WEBUI_BUILD_REV UBOS_WEBUI_BUILD_TIME;
            };

            # redacted-rtt: shared RTT backend. `log` (default) has no current
            # consumer but is still a supported mode of the crate; `defmt`
            # is used by both firmware/redacted and firmware/redacted. The two
            # are mutually exclusive (each defines its own CHAN_NAME), so
            # they get separate clippy/build checks rather than one
            # `--all-features` pass.
            clippy-redacted-rtt = riscvCraneLib.cargoClippy {
              inherit src;
              cargoArtifacts = riscvredactedRttArtifacts;
              cargoExtraArgs = "-p redacted-rtt --target riscv64gc-unknown-none-elf";
              cargoClippyExtraArgs = "-- -D warnings";
              doInstallCargoArtifacts = false;
              CARGO_TARGET_RISCV64GC_UNKNOWN_NONE_ELF_RUSTFLAGS = "-C panic=abort";
            };

            clippy-redacted-rtt-defmt = riscvCraneLib.cargoClippy {
              inherit src;
              cargoArtifacts = riscvredactedRttDefmtArtifacts;
              cargoExtraArgs = "-p redacted-rtt --target riscv64gc-unknown-none-elf --no-default-features --features defmt";
              cargoClippyExtraArgs = "-- -D warnings";
              doInstallCargoArtifacts = false;
              CARGO_TARGET_RISCV64GC_UNKNOWN_NONE_ELF_RUSTFLAGS = "-C panic=abort";
            };

            build-redacted-rtt = riscvCraneLib.cargoBuild {
              inherit src;
              cargoArtifacts = null;
              cargoBuildCommand = "cargo build --release";
              cargoExtraArgs = "-p redacted-rtt --target riscv64gc-unknown-none-elf";
              CARGO_TARGET_RISCV64GC_UNKNOWN_NONE_ELF_RUSTFLAGS = "-C panic=abort";
            };

            build-redacted-rtt-defmt = riscvCraneLib.cargoBuild {
              inherit src;
              cargoArtifacts = null;
              cargoBuildCommand = "cargo build --release";
              cargoExtraArgs = "-p redacted-rtt --target riscv64gc-unknown-none-elf --no-default-features --features defmt";
              CARGO_TARGET_RISCV64GC_UNKNOWN_NONE_ELF_RUSTFLAGS = "-C panic=abort";
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
              cargoArtifacts = loadTestArtifactsDev;
              CARGO_PROFILE = "";
              strictDeps = true;
              doInstallCargoArtifacts = false;
            };

            fast-host =
              pkgs.runCommand "ubos-main-fast-host-tests"
                {
                  nativeBuildInputs = [
                    test-embedded-i2c-unix
                    test-mining-protocol
                    test-ubos-asic
                    test-stratum-client
                    test-stratum-v1
                    test-stratum-v2
                    test-ubos-api
                    test-ubos-auth
                    test-ubos-config
                    test-ubos-config-std
                    test-ubos-eeprom
                    test-ubos-events
                    test-ubos-cooling
                    test-ubos-http
                    test-ubos-machine
                    test-ubos-manager
                    test-ubos-metrics
                    test-ubos-mining
                    test-ubos-net
                    test-ubos-psu
                    test-ubos-snapshot
                    test-ubos-temperature
                    test-ubos-thermal
                    test-load-test
                  ];
                }
                ''
                  mkdir -p "$out"
                  printf '%s\n' \
                    "ubos-main fast host test aggregate" \
                    "" \
                    "Building this check forces the isolated host-testable crate checks to build/run." \
                    "See docs/TESTING.md for the layer policy." \
                    > "$out/README"
                '';

            web-host =
              pkgs.runCommand "ubos-main-web-host-tests"
                {
                  nativeBuildInputs = [
                    test-ubos-webui
                    test-host-runner
                  ];
                }
                ''
                  mkdir -p "$out"
                  printf '%s\n' \
                    "ubos-main web/host-runner test aggregate" \
                    "" \
                    "Building this check forces the web UI and native host-runner tests to build/run." \
                    "These checks require the flake-provided esbuild dependency." \
                    > "$out/README"
                '';

            firmware-builds =
              pkgs.runCommand "ubos-main-firmware-builds"
                {
                  nativeBuildInputs = [
                    build-bmm-pico
                    build-bmm-pico-no-display
                    build-rp2040
                    build-rp2040-no-display
                    build-rp2350
                    build-rp2350-no-display
                    build-hashnode
                    build-stm32f407
                    build-stm32f407-no-display
                    build-stm32f439
                    build-stm32f439-no-display
                    build-esp32s3
                    build-esp32s3-no-display
                    build-redacted
                    build-rp2350-bootloader
                    build-rp2040-bootloader
                    build-esp32s3-bootloader
                  ];
                }
                ''
                  mkdir -p "$out"
                  printf '%s\n' \
                    "ubos-main firmware build aggregate" \
                    "" \
                    "Building this check forces all firmware release build checks to build." \
                    "It covers default features and --no-default-features for each board." \
                    > "$out/README"
                '';

            # Fail the build on an unused dependency so dead deps cannot
            # accumulate. Runs over the cleaned source in default mode, which
            # parses every Cargo.toml and greps its source, so it needs no
            # network and no Cargo.lock resolution. Optional deps wired only
            # into a feature are exempted per crate via the
            # [package.metadata.cargo-machete] ignored list.
            cargo-machete =
              pkgs.runCommand "cargo-machete"
                {
                  nativeBuildInputs = [ pkgs.cargo-machete ];
                }
                ''
                  cargo-machete ${src}
                  touch $out
                '';

            # The OTA image-size limit is hard-coded in several languages that
            # cannot share a definition: slotMaxBytes/esp32s3SlotBytes here,
            # the OTA_MAX_IMAGE_BYTES default in ubos-api, OTA_MAX_BYTES in the
            # webui, and Ota::MAX_IMAGE_BYTES in each board's config.rs. Grep
            # each source for the literal it must contain and fail on drift, so
            # the slot-size build guards, the per-board configs, and the
            # client-side rejection always agree on one number per board.
            ota-image-limit = pkgs.runCommand "ota-image-limit" { } ''
              check() {
                if ! grep -qF "$2" "$1"; then
                  echo "OTA image limit drift: $1 lacks '$2'" >&2
                  exit 1
                fi
              }
              # Shared RP ACTIVE-slot budget: the ubos-api default, the web-UI
              # fallback, and the four RP boards all pin slotMaxKiB * 1024.
              check ${./ubos/ubos-api/src/system.rs}     '${toString slotMaxKiB} * 1024'
              check ${./ubos/ubos-webui/src/index.js}    '${toString slotMaxKiB} * 1024'
              check ${./firmware/rp2040/src/config.rs}   '${toString slotMaxKiB} * 1024'
              check ${./firmware/rp2350/src/config.rs}   '${toString slotMaxKiB} * 1024'
              check ${./firmware/bmm-pico/src/config.rs} '${toString slotMaxKiB} * 1024'
              check ${./firmware/hashnode/src/config.rs} '${toString slotMaxKiB} * 1024'
              # esp32s3: the 3 MiB app slot, not the shared RP budget.
              check ${./firmware/esp32s3/src/config.rs}  '3 * 1024 * 1024'
              # redacted: APP slot plus the redacted v1 header. ota.rs additionally
              # asserts this equals APP_SIZE + AUP_HEADER_SIZE at compile time.
              check ${./firmware/redacted/src/config.rs}     '0x001C_0000 + 0xE0'
              touch $out
            '';

            # Nix formatting gate. In the monorepo this was owned by a
            # repo-root treefmt; as a standalone repo the flake must
            # gate its own .nix files so `nix flake check` stays the
            # single source of truth. Uses nixfmt, the same RFC-style
            # formatter treefmt drove.
            nixfmt =
              pkgs.runCommand "nixfmt-check"
                {
                  nativeBuildInputs = [ pkgs.nixfmt ];
                }
                ''
                  find ${./.} -name '*.nix' -print0 | xargs -0 nixfmt --check
                  touch $out
                '';

            # The patched probe-rs-tools-server is consumed only by the
            # NUC's nixosConfiguration, which nix flake check never builds,
            # so a patch that no longer applies or a probe-rs build break
            # would otherwise keep CI green. Realize the derivation here so
            # the check stage is its gate.
            probe-rs-tools-server = pkgs.probe-rs-tools-server;
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

          # Production firmware images for flashing via the WebUI, and combined
          # BOOTSEL UF2 images for drag-drop flashing via USB mass-storage.
          #
          # Build with a version number:
          #   UBOS_FW_VERSION=42 nix build --impure .#rp2350-image
          # This produces result/rp2350-v42.bin.
          #
          #   UBOS_FW_VERSION=42 nix build --impure .#rp2350-uf2
          # This produces result/rp2350-v42.uf2.
          #
          # A pure build (no --impure) defaults to version 1 because
          # builtins.getEnv returns "" in the Nix sandbox; the sandbox
          # strips all caller environment variables.
          packages = rec {
            rp2350-image = rp2350Image;
            bmm-pico-image = bmmPicoImage;
            rp2040-image = rp2040Image;
            hashnode-image = hashnodeImage;
            rp2350-uf2 = rp2350Uf2;
            bmm-pico-uf2 = bmmPicoUf2;
            rp2040-uf2 = rp2040Uf2;
            hashnode-uf2 = hashnodeUf2;

            # KiCad automation for the bmm-adapter fabrication job
            # (`nix run .#kibot`). DRC and ERC are gated separately by
            # the kicad-cli checks; kibot only generates fab outputs.
            kiauto = pkgs.callPackage ./kiauto.nix { };
            kibot = pkgs.callPackage ./kibot.nix { inherit kiauto; };

            # Headless redacted Logic 2 capture lifecycle wrapper. Also the
            # entry point of the `logic` devShell.
            redacted-run = redactedRun;

            esp32s3-image = esp32s3Image;
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

          # Copy the built image or UF2 into the repo's build/ directory and
          # print the absolute path of the written file as the last stdout line.
          #
          # Usage:
          #   nix run .#rp2350-image                                -> build/rp2350-v1.bin
          #   UBOS_FW_VERSION=42 nix run --impure .#rp2350-image    -> build/rp2350-v42.bin
          #   UBOS_FW_VERSION=42 nix run --impure .#bmm-pico-image  -> build/bmm-pico-v42.bin
          #   nix run .#rp2350-uf2                                  -> build/rp2350-v1.uf2
          #   nix run .#bmm-pico-uf2                                -> build/bmm-pico-v1.uf2
          #   nix run .#rp2040-uf2                                  -> build/rp2040-v1.uf2
          #   nix run .#hashnode-uf2                                -> build/hashnode-v1.uf2
          #
          # The final stdout line is the absolute path to the written file.
          apps = {
            rp2350-image = {
              type = "app";
              program = toString (
                pkgs.writeShellScript "rp2350-image-to-build" ''
                  set -euo pipefail
                  root="$(${pkgs.git}/bin/git rev-parse --show-toplevel)"
                  dest="$root/build"
                  mkdir -p "$dest"
                  # The package $out holds exactly one rp2350-v<N>.bin.
                  src_bin="$(echo ${rp2350Image}/*.bin)"
                  name="$(basename "$src_bin")"
                  install -m644 "$src_bin" "$dest/$name"
                  echo "rp2350 image v${fwVersion} written to:" >&2
                  echo "$dest/$name"
                ''
              );
            };
            bmm-pico-image = {
              type = "app";
              program = toString (
                pkgs.writeShellScript "bmm-pico-image-to-build" ''
                  set -euo pipefail
                  root="$(${pkgs.git}/bin/git rev-parse --show-toplevel)"
                  dest="$root/build"
                  mkdir -p "$dest"
                  # The package $out holds exactly one bmm-pico-v<N>.bin.
                  src_bin="$(echo ${bmmPicoImage}/*.bin)"
                  name="$(basename "$src_bin")"
                  install -m644 "$src_bin" "$dest/$name"
                  echo "bmm-pico image v${fwVersion} written to:" >&2
                  echo "$dest/$name"
                ''
              );
            };
            rp2040-image = {
              type = "app";
              program = toString (
                pkgs.writeShellScript "rp2040-image-to-build" ''
                  set -euo pipefail
                  root="$(${pkgs.git}/bin/git rev-parse --show-toplevel)"
                  dest="$root/build"
                  mkdir -p "$dest"
                  # The package $out holds exactly one rp2040-v<N>.bin.
                  src_bin="$(echo ${rp2040Image}/*.bin)"
                  name="$(basename "$src_bin")"
                  install -m644 "$src_bin" "$dest/$name"
                  echo "rp2040 image v${fwVersion} written to:" >&2
                  echo "$dest/$name"
                ''
              );
            };
            rp2350-uf2 = {
              type = "app";
              program = toString (
                pkgs.writeShellScript "rp2350-uf2-to-build" ''
                  set -euo pipefail
                  root="$(${pkgs.git}/bin/git rev-parse --show-toplevel)"
                  dest="$root/build"
                  mkdir -p "$dest"
                  # The package $out holds exactly one rp2350-v<N>.uf2.
                  src_uf2="$(echo ${rp2350Uf2}/*.uf2)"
                  name="$(basename "$src_uf2")"
                  install -m644 "$src_uf2" "$dest/$name"
                  echo "rp2350 UF2 v${fwVersion} written to:" >&2
                  echo "$dest/$name"
                ''
              );
            };
            bmm-pico-uf2 = {
              type = "app";
              program = toString (
                pkgs.writeShellScript "bmm-pico-uf2-to-build" ''
                  set -euo pipefail
                  root="$(${pkgs.git}/bin/git rev-parse --show-toplevel)"
                  dest="$root/build"
                  mkdir -p "$dest"
                  # The package $out holds exactly one bmm-pico-v<N>.uf2.
                  src_uf2="$(echo ${bmmPicoUf2}/*.uf2)"
                  name="$(basename "$src_uf2")"
                  install -m644 "$src_uf2" "$dest/$name"
                  echo "bmm-pico UF2 v${fwVersion} written to:" >&2
                  echo "$dest/$name"
                ''
              );
            };
            rp2040-uf2 = {
              type = "app";
              program = toString (
                pkgs.writeShellScript "rp2040-uf2-to-build" ''
                  set -euo pipefail
                  root="$(${pkgs.git}/bin/git rev-parse --show-toplevel)"
                  dest="$root/build"
                  mkdir -p "$dest"
                  # The package $out holds exactly one rp2040-v<N>.uf2.
                  src_uf2="$(echo ${rp2040Uf2}/*.uf2)"
                  name="$(basename "$src_uf2")"
                  install -m644 "$src_uf2" "$dest/$name"
                  echo "rp2040 UF2 v${fwVersion} written to:" >&2
                  echo "$dest/$name"
                ''
              );
            };
            hashnode-uf2 = {
              type = "app";
              program = toString (
                pkgs.writeShellScript "hashnode-uf2-to-build" ''
                  set -euo pipefail
                  root="$(${pkgs.git}/bin/git rev-parse --show-toplevel)"
                  dest="$root/build"
                  mkdir -p "$dest"
                  # The package $out holds exactly one hashnode-v<N>.uf2.
                  src_uf2="$(echo ${hashnodeUf2}/*.uf2)"
                  name="$(basename "$src_uf2")"
                  install -m644 "$src_uf2" "$dest/$name"
                  echo "hashnode UF2 v${fwVersion} written to:" >&2
                  echo "$dest/$name"
                ''
              );
            };
            esp32s3-image = {
              type = "app";
              program = toString (
                pkgs.writeShellScript "esp32s3-image-to-build" ''
                  set -euo pipefail
                  root="$(${pkgs.git}/bin/git rev-parse --show-toplevel)"
                  dest="$root/build"
                  mkdir -p "$dest"
                  # The package $out holds exactly one esp32s3-v<N>.bin.
                  src_bin="$(echo ${esp32s3Image}/*.bin)"
                  name="$(basename "$src_bin")"
                  install -m644 "$src_bin" "$dest/$name"
                  echo "esp32s3 image v${fwVersion} written to:" >&2
                  echo "$dest/$name"
                ''
              );
            };
          };
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
