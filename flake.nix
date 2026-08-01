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
# shells and the `nix develop .#arm` entry point. The `lib.modules`
# output at the bottom re-exports those modules for a downstream repo
# that builds its own workspace against these crates.
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
    # ESP-IDF packaged for Nix; used only to build the esp32s3 second-stage
    # bootloader with rollback enabled. No esp-rs prebuilt rollback bootloader
    # exists, so we compile our own. It intentionally does NOT follow our
    # nixpkgs: its esp-idf derivation pins python310, which nixos-unstable has
    # dropped, so it must keep its own locked nixpkgs.
    nixpkgs-esp-dev.url = "github:mirrexagon/nixpkgs-esp-dev";
    # Vendored SV2 library workspace (micro-sv2/). It is its own flake with
    # its own locked inputs, kept independent so it still builds standalone
    # and stays borrowable; ubos gates it by composing its checks below
    # rather than re-deriving crane builds. Deliberately not `follows`ed onto
    # our nixpkgs: it pins nixos-25.11 and builds as upstream tested.
    micro-sv2.url = "path:./micro-sv2";
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
      rust-overlay,
      crane,
      nixpkgs-esp-dev,
      micro-sv2,
    }@inputs:
    let
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

      # probe-rs tooling: the patched build, the remote patch variant,
      # and the overlay exposing them plus the probe-rs-runner
      # wrapper. A downstream repo carrying its own silicon-support
      # series re-imports this module with `extraPatches`; see the
      # `lib.modules` output below.
      probeRs = import ./nix/probe-rs.nix { };
      probeRsOverlay = probeRs.overlay;
    in
    # x86_64-linux only: the esp-rs Xtensa toolchain and the espressif
    # cross-gcc are prebuilt x86_64 tarballs, so the per-system checks
    # and shells cannot work elsewhere.
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
          ;

        # ESP32-S3 build universe for the esp32s3 board: the Xtensa
        # toolchain, its crane instance, and the second-stage
        # bootloader. See nix/esp.nix.
        esp = import ./nix/esp.nix {
          inherit pkgs system;
          inherit (inputs) crane nixpkgs-esp-dev;
        };
        inherit (esp)
          espToolchain
          espCraneLib
          espCargoVendorDir
          xtensaEspToolchain
          esp32s3Bootloader
          ;

        # Pinned upstream crates with local patch files applied; the
        # workspace `[patch]` table resolves them at patches/<crate>.
        # See nix/patched-crates.nix.
        patchedCrates = import ./nix/patched-crates.nix { inherit pkgs; };

        # Cleaned source trees for the crane builds. See nix/source.nix.
        sources = import ./nix/source.nix {
          inherit pkgs craneLib patchedCrates;
        };
        inherit (sources) src srcFor;

        # Workspace package names present in this tree, from crate-graph.
        # The checks and images modules gate every firmware crate on its
        # membership, so a tree carrying a subset of the boards evaluates
        # with those gates absent instead of dangling on missing sources.
        members = sources.crateGraph.members;

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
            patchedCrates
            ;
        };

        # Shared dependency artifacts for the clippy, test, and build
        # checks. See nix/deps.nix for the bag-splitting rationale.
        deps = import ./nix/deps.nix {
          inherit
            pkgs
            src
            craneLib
            helpers
            bmmCraneLib
            espCraneLib
            espCargoVendorDir
            xtensaEspToolchain
            bmmCc
            members
            ;
        };

        # Maximum flash bytes allowed before CI fails the size guard.
        # The value is the embassy-boot ACTIVE partition budget. The 2 MiB
        # flash is split into a 128 KiB bootloader, a 4 KiB state word, an
        # 8 KiB config store at the tail, and the remaining 1908 KiB shared
        # 50/50 between the ACTIVE and DFU slots (ACTIVE 952 KiB, DFU one
        # page larger at 956 KiB; see firmware/rp2350-bootloader/memory.x).
        # rp2350 and bmm-pico reuse the rp2350-bootloader with this
        # layout, and rp2040 mirrors it, so an image over 952 KiB does
        # not fit the slot.
        # The kibibyte count is the source of truth. The ota-image-limit
        # check greps both source files for "<slotMaxKiB> * 1024" so a drift
        # in OTA_MAX_IMAGE_BYTES (ubos/ubos-api/src/system.rs) or
        # OTA_MAX_BYTES (ubos/ubos-webui/src/index.js) fails CI.
        slotMaxKiB = 952;
        slotMaxBytes = slotMaxKiB * 1024;

        # esp32s3 OTA app-slot budget. The 8 MiB flash is partitioned into
        # two equal 3 MiB app slots (ota_0/ota_1 in the board's partition
        # table), so the image the OTA endpoint writes into the inactive
        # slot may be up to 3 MiB — larger than the RP ACTIVE budget. Keep
        # in sync with Ota::MAX_IMAGE_BYTES in
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
            members
            ;
        };

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
              members
              ;
          }
          # Vendored micro-sv2 is its own workspace the root crane build does
          # not cover, so gate it by re-exporting its flake's checks. The
          # clippy- prefix routes to ci-clippy; micro-sv2-formatting is named
          # in nix/ci.nix's ci-format set; the rest fall into ci-check.
          // {
            "clippy-micro-sv2" = micro-sv2.checks.${system}.clippy;
            "micro-sv2-test" = micro-sv2.checks.${system}.test;
            "micro-sv2-formatting" = micro-sv2.checks.${system}.formatting;
          };

        # Interactive development shells; see nix/shells.nix.
        devShells = import ./nix/shells.nix {
          inherit
            pkgs
            toolchains
            helpers
            esp
            ;
        };

        # Production firmware images and combined BOOTSEL UF2 images;
        # see nix/images.nix for the UBOS_FW_VERSION build instructions.
        packages =
          images.packages
          // {
            # GC-root anchor for the armv5tel-musl gcc for the BMM
            # miniminer, which cache.nixos.org does not carry.
            # `nix build .#cross-toolchains` pins the whole closure so
            # nix-collect-garbage keeps it. The rust toolchains
            # substitute from the rust-overlay cache and the Xtensa
            # compiler is espup-managed, so neither needs an anchor here.
            cross-toolchains = pkgs.linkFarm "ubos-cross-toolchains" {
              bmm-cc = bmmCc;
            };

            # The esp32s3 second-stage bootloader; see nix/esp.nix.
            esp32s3-bootloader = esp32s3Bootloader;
          }
          # The CI stage groups partitioning `checks`; see nix/ci.nix.
          // import ./nix/ci.nix {
            inherit pkgs;
            checks = self.checks.${system};
          };

        # Copy apps: `nix run .#<image>` writes the built image or UF2
        # into the repo's build/ directory. See nix/images.nix.
        apps = images.apps;

        # `nix fmt` entry point, matching the nixfmt check in
        # nix/checks.nix.
        formatter = pkgs.nixfmt;
      }
    ))
    // {
      # Reusable wiring for a downstream repo that consumes this one as
      # a flake input and builds a workspace of its own against these
      # crates. Exporting the modules rather than letting the consumer
      # copy them is what keeps the two trees' toolchain pins, source
      # filters, dependency bags, and CI grouping from drifting apart;
      # every module already takes its inputs as explicit arguments, so
      # nothing here is a wrapper.
      #
      # `source` and `crate-graph` take a `root`, so a consumer points
      # them at its own tree. `patched-crates` deliberately does not:
      # the patch files live here, and its whole job is to materialize
      # the pins this repo's `[patch]` table resolves against.
      lib.modules = {
        ci = ./nix/ci.nix;
        crate-graph = ./nix/crate-graph.nix;
        deps = ./nix/deps.nix;
        guards = ./nix/guards.nix;
        helpers = ./nix/lib.nix;
        patched-crates = ./nix/patched-crates.nix;
        probe-rs = ./nix/probe-rs.nix;
        source = ./nix/source.nix;
        toolchains = ./nix/toolchains.nix;
      };

      # The probe-rs overlay as this repo instantiates it. A consumer
      # carrying its own silicon-support series imports `lib.modules
      # .probe-rs` with `extraPatches` and uses the overlay that comes
      # back instead of this one.
      overlays.probe-rs = probeRsOverlay;
    };
}
