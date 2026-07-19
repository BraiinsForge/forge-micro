# The ubos-main bare-metal Rust firmware workspace flake.
#
# Why: each board needs a specific Rust target and, for the ESP32-S3,
# an Xtensa toolchain that is not in nixpkgs. This flake pins both so
# a clean checkout compiles without manual rustup or espup setup.
#
# What: the outputs below wire per-board toolchains, cleaned sources,
# the per-crate checks, firmware images, and the interactive dev
# shells together. Each block is a thin import of a nix/ module and
# names the module it comes from; see nix/shells.nix for the dev
# shells and the `nix develop .#arm` / `.#esp` entry points.
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
      # x86_64-linux only: the esp-rs Xtensa toolchain and the espressif
      # cross-gcc are prebuilt x86_64 tarballs, so the per-system checks
      # and shells cannot work elsewhere. The aarch64 NUC closure is not
      # a per-system output; it comes from the nixosConfiguration and the
      # explicit packages.aarch64-linux.sd-image below.
      (flake-utils.lib.eachSystem [ "x86_64-linux" ] (
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
            espToolchain
            espCraneLib
            espCargoVendorDir
            xtensaEspToolchain
            esp32s3Bootloader
            ;

          # Cleaned source trees for the crane builds. See nix/source.nix.
          sources = import ./nix/source.nix {
            inherit pkgs craneLib;
          };
          inherit (sources) src srcFor loadTestSrc;

          # Slot-size and pool-worker guard text, rendered from one
          # function so the per-board copies cannot drift apart. See
          # nix/guards.nix.
          guards = import ./nix/guards.nix { };

          # Cross-CC and webui build-env helpers shared by the checks,
          # images, and shells modules. See nix/lib.nix.
          helpers = import ./nix/lib.nix {
            inherit
              pkgs
              webuiBuildEnv
              espToolchain
              xtensaEspToolchain
              ;
          };

          # Shared dependency artifacts for the clippy, test, and build
          # checks. See nix/deps.nix for the bag-splitting rationale.
          deps = import ./nix/deps.nix {
            inherit
              pkgs
              src
              loadTestSrc
              craneLib
              helpers
              bmmCraneLib
              espCraneLib
              espCargoVendorDir
              xtensaEspToolchain
              bmmCc
              ;
          };

          # Optional private-overlay checks, present only in the internal
          # tree. The public mirror does not ship nix/private-checks.nix, so
          # this evaluates to an empty attrset there instead of failing.
          privateChecks =
            let
              f = ./nix/private-checks.nix;
            in
            if builtins.pathExists f then
              import f {
                inherit
                  pkgs
                  helpers
                  craneLib
                  riscvCraneLib
                  riscvGcc
                  riscvDefmtRustflags
                  srcFor
                  src
                  ;
              }
            else
              { };

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
              srcFor
              deps
              craneLib
              espCraneLib
              espCargoVendorDir
              xtensaEspToolchain
              guards
              helpers
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
                srcFor
                loadTestSrc
                deps
                craneLib
                bmmCraneLib
                espCraneLib
                bmmCc
                espToolchain
                xtensaEspToolchain
                espCargoVendorDir
                esp32s3Bootloader
                guards
                helpers
                slotMaxKiB
                slotMaxBytes
                esp32s3SlotBytes
                ;
            }
            # Optional private-overlay checks; see privateChecks above.
            // privateChecks
            # The bmm-adapter DRC/ERC gates; see nix/kicad.nix.
            // kicad.checks
            # The SD image is a `packages` output, so no check covered it and
            # an evaluation regression - a nixpkgs bump whose device-tree
            # module the nixos-raspberrypi kernel cannot satisfy, say - passed
            # CI while failing `nix flake check`. Force a full NixOS evaluation
            # of the cross-built image here so that gap closes. The image
            # itself is never built: unsafeDiscardOutputDependency keeps the
            # evaluation but drops the multi-gigabyte build from the closure.
            // {
              sd-image-eval = pkgs.runCommand "sd-image-eval" { } ''
                echo ${builtins.unsafeDiscardOutputDependency (sdImageFor system).drvPath} > $out
              '';
            };

          # Interactive development shells; see nix/shells.nix. The
          # private overlay nix/private-shells.nix contributes extra
          # shells when present, merged in only if the file exists —
          # absent from the public mirror, same idiom as privateChecks.
          devShells =
            (import ./nix/shells.nix {
              inherit
                pkgs
                toolchains
                esp
                redacted
                probeRsHost
                probeRsToken
                helpers
                ;
              deployRsPackage = deploy-rs.packages.${system}.default;
            })
            // (
              let
                f = ./nix/private-shells.nix;
              in
              if builtins.pathExists f then
                import f {
                  inherit
                    pkgs
                    toolchains
                    helpers
                    probeRsHost
                    probeRsToken
                    ;
                }
              else
                { }
            );

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
            # CI stage groups partitioning `checks`; see nix/ci.nix.
            // (import ./nix/ci.nix {
              inherit pkgs;
              checks = self.checks.${system};
            });

          # Copy apps: `nix run .#<image>` writes the built image or UF2
          # into the repo's build/ directory. See nix/images.nix.
          apps =
            images.apps
            # `nix run .#publish` emits the gated public mirror. Present
            # only in the internal tree, guarded like privateChecks above:
            # the public mirror ships no publish/, so this degrades away.
            // (
              if builtins.pathExists ./publish/app.nix then
                { publish = import ./publish/app.nix { inherit pkgs; }; }
              else
                { }
            );

          # `nix fmt` entry point, matching the nixfmt check in
          # nix/checks.nix.
          formatter = pkgs.nixfmt;
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
