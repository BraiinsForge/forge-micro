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

# Per-crate gates and static guards, under the Verification rule in
# CLAUDE.md: a crate that is not in `checks` is not gated.
#
# The regular shapes are generated from the tables below, so gating a
# new host crate is one line in `hostClippyChecks`, plus
# `hostTestChecks` when it has unit tests, and a new board is one entry
# in `boards`. The irregular checks stay hand-written because
# forcing them through the tables would need a knob per quirk.
#
# The `[patch]` override crates from nix/patched-crates.nix carry no
# clippy or build derivation of their own: they are upstream crates we
# do not author, and gating them would tie the build to upstream lint
# cleanliness we do not control. Every firmware build below exercises
# them transitively, so a real breakage still fails the gate.
{
  pkgs,
  src,
  srcFor,
  deps,
  patchedCratesOverlay,
  craneLib,
  units,
  # See flake.nix.
  webuiEnv,
  webuiHostEnv,
  bmmCraneLib,
  espCraneLib,
  bmmCc,
  espToolchain,
  xtensaEspToolchain,
  espCargoVendorDir,
  esp32s3Bootloader,
  guards,
  helpers,
  rustVersion,
  slotMaxKiB,
  slotMaxBytes,
  esp32s3SlotBytes,
  boardParity,
  brands,
  brandGates,
  # Workspace package names present in the tree, from crate-graph. Every
  # firmware gate below filters itself out when its crate is not a
  # member, so a tree carrying a subset of the boards evaluates with
  # those gates absent instead of dangling on sources it does not have.
  # In this repo every board is a member, so the filters are the identity
  # and gate all of them.
  members,
  # The dependency walk over the manifests, from nix/crate-graph.nix,
  # for the crate-layers gate.
  crateGraph,
}:
let
  inherit (pkgs) lib;
  inherit (helpers)
    webuiDist
    webuiDistEnv
    espCrossEnv
    espRustflags
    ;
  root = ../.;

  claudeMdMaxLines = 200;

  # The fewest instruction files instruction-paths below expects to
  # reach. It exists to catch a scan that reads nothing, not to fix the
  # count, so it sits far under what the tree holds and moves only if
  # the tree ever holds fewer.
  minInstructionFiles = 5;

  # The host gates run with the fee switch on, the wider state the
  # fee-slot tests and the ubos-api config body pin measure. A row that
  # states `fee = fee.off` gates the other state, and a board gate takes
  # the state its own cargo config states.
  fee = import ./fee.nix;
  feeEnv = name: fee.envFor (root + "/firmware/${name}");

  hasCrate = name: builtins.elem name members;

  # Host-side clippy: one row per check, one plan per row. `tests` is off
  # for crates whose tests do not build on the host feature set, or link
  # only under the features their test row names. `fee` is on unless the
  # row says `fee.off`. No gate takes the build id: it changes with every
  # commit, and a gate stamped with it would rebuild on every commit for
  # an id nothing reads. Only the images in nix/images.nix carry it.
  mkHostClippy =
    name: row:
    pkgs.linkFarmFromDrvs name (units.unitsOf (units.hostPlan ({ fee = fee.on; } // row)) row.crate);

  # Host-side unit tests, dev profile so debug_assert! and
  # overflow-checks fire in CI.
  mkHostTest =
    name: row:
    pkgs.linkFarmFromDrvs name (units.testsOf (units.hostPlan ({ fee = fee.on; } // row)) row.crate);

  hostClippyChecks = builtins.mapAttrs mkHostClippy {
    # The schema feature adds the JsonSchema derives on the units, so
    # clippy reads them too.
    clippy-ubos-core = {
      crate = "ubos-core";
      features = [ "schema" ];
    };
    clippy-ubos-ota.crate = "ubos-ota";
    clippy-ubos-led.crate = "ubos-led";
    clippy-ubos-http = {
      crate = "ubos-http";
      tests = false;
    };
    # The schema feature adds the JsonSchema derives the OpenAPI
    # document is built from, so clippy reads them too.
    clippy-ubos-api = {
      crate = "ubos-api";
      features = [ "schema" ];
      tests = false;
    };
    clippy-ubos-auth.crate = "ubos-auth";
    clippy-ubos-build.crate = "ubos-build";
    clippy-ubos-metrics.crate = "ubos-metrics";
    clippy-ubos-mdns.crate = "ubos-mdns";
    clippy-ubos-net.crate = "ubos-net";
    clippy-ubos-prometheus.crate = "ubos-prometheus";
    clippy-ubos-psu.crate = "ubos-psu";
    clippy-ubos-snapshot.crate = "ubos-snapshot";
    clippy-ubos-eeprom.crate = "ubos-eeprom";
    clippy-embedded-i2c-unix.crate = "embedded-i2c-unix";
    clippy-ubos-mining.crate = "ubos-mining";
    clippy-mining-protocol.crate = "mining-protocol";
    clippy-ubos-asic.crate = "ubos-asic";
    clippy-ubos-tuner.crate = "ubos-tuner";
    clippy-stratum-v1.crate = "stratum-v1";
    clippy-stratum-v2.crate = "stratum-v2";
    clippy-transitions.crate = "transitions";
    clippy-stratum-client.crate = "stratum-client";
    clippy-ubos-temperature.crate = "ubos-temperature";
    clippy-ubos-display = {
      crate = "ubos-display";
      tests = false;
    };
    clippy-ubos-logging = {
      crate = "ubos-logging";
      tests = false;
    };
    clippy-embassy-net-afpacket = {
      crate = "embassy-net-afpacket";
      tests = false;
    };
    clippy-embedded-serial-unix = {
      crate = "embedded-serial-unix";
      tests = false;
    };
    clippy-serial-baudrate = {
      crate = "serial-baudrate";
      tests = false;
    };
    clippy-ubos-machine = {
      crate = "ubos-machine";
      tests = false;
    };
    clippy-ubos-main-host = {
      crate = "ubos-main-host";
      tests = false;
    };
    clippy-ubos-webui.crate = "ubos-webui";
    clippy-ubos-thermal = {
      crate = "ubos-thermal";
      tests = false;
    };
    clippy-ubos-cooling = {
      crate = "ubos-cooling";
      tests = false;
    };
    clippy-ubos-manager.crate = "ubos-manager";
    clippy-ubos-config.crate = "ubos-config";
    clippy-ubos-config-std = {
      crate = "ubos-config";
      features = [
        "std"
        "schema"
      ];
    };
    clippy-ubos-events = {
      crate = "ubos-events";
      tests = false;
    };
    clippy-ubos-events-std = {
      crate = "ubos-events";
      features = [ "std" ];
    };
    clippy-host-runner.crate = "host-runner";
  };

  hostTestRows = {
    test-ubos-core.crate = "ubos-core";
    test-ubos-mdns.crate = "ubos-mdns";
    test-ubos-net.crate = "ubos-net";
    test-ubos-psu.crate = "ubos-psu";
    test-ubos-snapshot.crate = "ubos-snapshot";
    test-ubos-eeprom.crate = "ubos-eeprom";
    test-embedded-i2c-unix.crate = "embedded-i2c-unix";
    test-ubos-auth.crate = "ubos-auth";
    test-ubos-build.crate = "ubos-build";
    test-ubos-metrics.crate = "ubos-metrics";
    test-ubos-mining.crate = "ubos-mining";
    test-stratum-v1.crate = "stratum-v1";
    test-stratum-v2.crate = "stratum-v2";
    test-transitions.crate = "transitions";
    test-stratum-client.crate = "stratum-client";
    test-mining-protocol.crate = "mining-protocol";
    test-ubos-asic.crate = "ubos-asic";
    test-ubos-tuner.crate = "ubos-tuner";
    test-ubos-machine.crate = "ubos-machine";
    test-ubos-temperature.crate = "ubos-temperature";
    test-ubos-http = {
      crate = "ubos-http";
      noDefaultFeatures = true;
    };
    test-ubos-api = {
      crate = "ubos-api";
      noDefaultFeatures = true;
      features = [ "schema" ];
    };
    test-ubos-webui.crate = "ubos-webui";
    test-host-runner.crate = "host-runner";
    test-ubos-thermal = {
      crate = "ubos-thermal";
      noDefaultFeatures = true;
    };
    test-ubos-cooling = {
      crate = "ubos-cooling";
      noDefaultFeatures = true;
    };
    test-ubos-manager.crate = "ubos-manager";
    test-ubos-config.crate = "ubos-config";
    # The one host gate with the fee switch off, so an off image's
    # pool stack is tested too; see `fee` above.
    test-ubos-config-off = {
      crate = "ubos-config";
      fee = fee.off;
    };
    test-ubos-config-std = {
      crate = "ubos-config";
      features = [
        "std"
        "schema"
      ];
    };
    test-ubos-events = {
      crate = "ubos-events";
      features = [ "std" ];
    };
  };
  hostTestChecks = builtins.mapAttrs mkHostTest hostTestRows;

  # The coverage checks of the host test rows; see `coverageSuite` in
  # nix/coverage.nix. ubos-tuner is left out, because its code is a trait
  # and its types, which only doc tests exercise, so its test run has
  # nothing to count.
  coverageChecks = units.coverageSuite {
    rows = lib.mapAttrs' (name: lib.nameValuePair (lib.removePrefix "test-" name)) (
      removeAttrs hostTestRows [ "test-ubos-tuner" ]
    );
    fee = fee.on;
    inherit (import ./coverage-floors.nix) floors;
    skipped = {
      # The file store builds only with the `std` feature, which these
      # two rows do not turn on. The `ubos-config-std` row scores it.
      ubos-config = [ "**/file_storage.rs" ];
      ubos-config-off = [ "**/file_storage.rs" ];
      # Each function of these two files is `main` or a task that runs
      # against the TAP interface or the serial port of a real host.
      host-runner = [
        "**/main.rs"
        "**/miner_serial.rs"
      ];
    };
  };

  # One row per ARM board, generating clippy-<name> and build-<name> with
  # the default feature set; the build runs the image guard over the
  # linked ELF.
  #
  # `board-coverage` below reads this unfiltered table. The filter is
  # what makes a row whose crate is absent disappear rather than fail,
  # so a gate reading the filtered `boards` could never see the row it
  # is looking for.
  boardTable = {
    forge-axe = {
      target = "thumbv8m.main-none-eabihf";
    };
    bmm-pico = {
      target = "thumbv8m.main-none-eabihf";
    };
    rp2040 = {
      target = "thumbv6m-none-eabi";
    };
    rp2350 = {
      target = "thumbv8m.main-none-eabihf";
    };
    hashnode = {
      target = "thumbv8m.main-none-eabihf";
    };
  };

  boards = lib.filterAttrs (name: _: hasCrate name) boardTable;

  # The firmware crates gated outside `boardTable`, each by a
  # hand-written check above. `board-coverage` asks whether a firmware
  # directory is gated at all, not whether it goes through the table, so
  # it needs them.
  handGatedBoards = [
    "esp32s3"
    "esp32s3-bootloader"
    "rp2040-bootloader"
    "rp2350-bootloader"
  ];

  # The script that reads a value out of the ELF of an image. The image
  # guard runs it to hold that each image exports the config schema hash.
  elfSymbol = pkgs.callPackage ../pkgs/elf-symbol { };
  # The export name of SCHEMA_HASH_SYMBOL in ubos-config's versions module.
  schemaHashSymbol = "UBOS_CONFIG_SCHEMA_HASH";

  # cargo check and cargo clippy stop short of linking the final ELF for
  # a binary crate, so they miss .bss and flash overflows; the guard
  # objcopies the ELF a board plan links and enforces the OTA slot budget.
  # It also reads the config schema hash out of the ELF, so an image whose
  # link lost the symbol fails its build check rather than the packing of
  # an update. The Footprint section of docs/STRUCTURE.md records the RAM
  # snapshot it protects, docs/OTA.md the flash layout and slot budget;
  # the guard text renders from nix/guards.nix.
  imageGuard =
    name: plan:
    pkgs.runCommand "build-${name}" { nativeBuildInputs = [ pkgs.gcc-arm-embedded ]; } (
      ''
        ${elfSymbol}/bin/elf-symbol ${plan}/bin/${name} ${schemaHashSymbol} > /dev/null
        bin=$(mktemp --suffix=.bin)
        arm-none-eabi-objcopy -O binary ${plan}/bin/${name} "$bin"
      ''
      + guards.imageGuard {
        inherit name;
        bin = ''"$bin"'';
        maxBytes = slotMaxBytes;
        cleanup = ''rm -f "$bin"; '';
      }
      + ''
        rm -f "$bin"
        touch "$out"
      ''
    );

  # A board's clippy and build checks in one fee state, under names that
  # end in `suffix`. Both are the one plan, the build with its image
  # guarded.
  mkBoardChecks =
    suffix: fee: name: board:
    let
      plan = units.boardPlan {
        inherit name fee;
        inherit (board) target;
      };
    in
    {
      "clippy-${name}${suffix}" = plan;
      "build-${name}${suffix}" = imageGuard name plan;
    };

  boardChecks = lib.concatMapAttrs (name: mkBoardChecks "" (feeEnv name) name) boards;

  # The other fee state, one board per ARM target that can carry the fee,
  # so the on path cannot rot in a tree whose images ship it off. The
  # build row measures the RAM a fourth session task costs.
  feeOnBoards = [
    "rp2040"
    "rp2350"
  ];
  feeOnChecks = lib.concatMapAttrs (mkBoardChecks "-on" fee.on) (
    lib.filterAttrs (name: _: lib.elem name feeOnBoards) boards
  );

  # The plan's own result holds only what it installs, which for a library
  # is nothing, so the check takes the crate's units instead.
  mkLibraryCheck =
    name: args: pkgs.linkFarmFromDrvs name (units.unitsOf (units.boardPlan args) args.name);

  # The same clippy/build pair as the boards, without an image guard,
  # since a bootloader carries no pool worker and has its own flash
  # region.
  mkBootloaderChecks =
    name: target:
    let
      plan = units.boardPlan { inherit name target; };
    in
    {
      "clippy-${name}" = plan;
      "build-${name}" = plan;
    };

  espCommon = espCrossEnv // {
    src = srcFor [ "esp32s3" ];
    cargoVendorDir = espCargoVendorDir;
    doInstallCargoArtifacts = false;
    nativeBuildInputs = [
      xtensaEspToolchain
    ];
  };

  rp2350BootloaderChecks = mkBootloaderChecks "rp2350-bootloader" "thumbv8m.main-none-eabihf";
  rp2040BootloaderChecks = mkBootloaderChecks "rp2040-bootloader" "thumbv6m-none-eabi";

  # No codegen bag to reuse; see the esp bag reason in nix/deps.nix. The
  # size guard must live here and not only in packages.esp32s3-image:
  # `nix flake check` builds checks.* but merely evaluates packages.*, so a
  # guard there would gate nothing.
  espBuildIn =
    env:
    espCraneLib.cargoBuild (
      espCommon
      // {
        cargoArtifacts = null;
        cargoBuildCommand = "cargo build --release";
        cargoExtraArgs = "-p esp32s3@0.1.0 --target xtensa-esp32s3-none-elf -Z build-std=core";
        nativeBuildInputs = espCommon.nativeBuildInputs ++ [ pkgs.espflash ];
        postBuild = ''
          elf=target/xtensa-esp32s3-none-elf/release/esp32s3
          bin=$(mktemp --suffix=.bin)
          # espflash reads/writes a user cache dir; keep it in the sandbox.
          export HOME="$TMPDIR"
          espflash save-image --chip esp32s3 --flash-size 8mb "$elf" "$bin"
        ''
        + guards.imageGuard {
          name = "esp32s3";
          bin = ''"$bin"'';
          maxBytes = esp32s3SlotBytes;
          budgetLabel = "OTA app slot";
          cleanup = ''rm -f "$bin"; '';
        }
        + ''
          rm -f "$bin"
        '';
      }
      // espRustflags
      // (webuiEnv "esp32s3")
      // env
    );

  # Grouped so they drop as a unit when the board is not a member, which
  # is what keeps a tree without it from pulling in an Xtensa toolchain
  # at all. Everything needing that toolchain is gated here for that
  # reason, the esp backend of ubos-ota and esp32s3-tsens included.
  espChecks = {
    # Our firmware crate name collides with the esp32s3 PAC crate pulled
    # in transitively; disambiguate by pinning to @0.1.0.
    clippy-esp32s3 = espCraneLib.cargoClippy (
      espCommon
      // {
        cargoArtifacts = deps.espArtifacts;
        cargoExtraArgs = "-p esp32s3@0.1.0 --target xtensa-esp32s3-none-elf -Z build-std=core";
        cargoClippyExtraArgs = "-- -D warnings";
      }
      // (webuiEnv "esp32s3")
      // feeEnv "esp32s3"
    );

    # The other fee state on Xtensa; see `feeOnChecks`.
    clippy-esp32s3-on = espCraneLib.cargoClippy (
      espCommon
      // {
        cargoArtifacts = deps.espArtifacts;
        cargoExtraArgs = "-p esp32s3@0.1.0 --target xtensa-esp32s3-none-elf -Z build-std=core";
        cargoClippyExtraArgs = "-- -D warnings";
      }
      // (webuiEnv "esp32s3")
      // fee.on
    );

    build-esp32s3 = espBuildIn (feeEnv "esp32s3");

    build-esp32s3-on = espBuildIn fee.on;

    # Gated so a broken sdkconfig fails CI. It pulls the esp-idf closure
    # on first build, cached until the nixpkgs-esp-dev input or the stub
    # project changes.
    build-esp32s3-bootloader = esp32s3Bootloader;

    clippy-esp32s3-tsens = espCraneLib.cargoClippy {
      src = srcFor [ "esp32s3-tsens" ];
      cargoArtifacts = deps.espArtifacts;
      cargoVendorDir = espCargoVendorDir;
      cargoExtraArgs = "-p esp32s3-tsens --target xtensa-esp32s3-none-elf -Z build-std=core";
      cargoClippyExtraArgs = "-- -D warnings";
      doInstallCargoArtifacts = false;
      RUST_SRC_PATH = "${espToolchain}/lib/rustlib/src/rust/library";
    };

    clippy-ubos-esp32 = espCraneLib.cargoClippy {
      src = srcFor [ "ubos-esp32" ];
      cargoArtifacts = deps.espArtifacts;
      cargoVendorDir = espCargoVendorDir;
      cargoExtraArgs = "-p ubos-esp32 --target xtensa-esp32s3-none-elf -Z build-std=core";
      cargoClippyExtraArgs = "-- -D warnings";
      doInstallCargoArtifacts = false;
      RUST_SRC_PATH = "${espToolchain}/lib/rustlib/src/rust/library";
    };
  };

in
hostClippyChecks
// hostTestChecks
// coverageChecks
// boardChecks
// feeOnChecks
// lib.optionalAttrs (hasCrate "rp2350-bootloader") rp2350BootloaderChecks
// lib.optionalAttrs (hasCrate "rp2040-bootloader") rp2040BootloaderChecks
// lib.optionalAttrs (hasCrate "esp32s3") espChecks
// rec {
  clippy-rp235x-sha256 = mkLibraryCheck "clippy-rp235x-sha256" {
    name = "rp235x-sha256";
    target = "thumbv8m.main-none-eabihf";
  };

  build-rp235x-sha256 = mkLibraryCheck "build-rp235x-sha256" {
    name = "rp235x-sha256";
    target = "thumbv8m.main-none-eabihf";
  };

  # The rp235xa half. The rp2040 board builds lint the rp2040 half,
  # since every unit compiles through `clippyRustc` in units.nix.
  clippy-ubos-rp = mkLibraryCheck "clippy-ubos-rp" {
    name = "ubos-rp";
    target = "thumbv8m.main-none-eabihf";
    features = [
      "rp235x"
      "embassy-rp/rp235xa"
      "embassy-rp/imagedef-none"
    ];
  };

  clippy-rp-bootloader = mkLibraryCheck "clippy-rp-bootloader" {
    name = "rp-bootloader";
    target = "thumbv8m.main-none-eabihf";
    features = [
      "embassy-rp/rp235xa"
    ];
  };

  build-ubos-firmware = mkLibraryCheck "build-ubos-firmware" {
    name = "ubos-firmware";
    target = "thumbv8m.main-none-eabihf";
  };

  clippy-ubos-firmware = mkLibraryCheck "clippy-ubos-firmware" {
    name = "ubos-firmware";
    target = "thumbv8m.main-none-eabihf";
  };

  clippy-host-runner-bmm = bmmCraneLib.cargoClippy (
    {
      src = srcFor [ "host-runner" ];
      cargoArtifacts = deps.bmmArtifacts;
      cargoExtraArgs = "-p host-runner --target arm-unknown-linux-musleabi";
      cargoClippyExtraArgs = "-- -D warnings";
      doInstallCargoArtifacts = false;
      nativeBuildInputs = [
        bmmCc
      ];
      CARGO_TARGET_ARM_UNKNOWN_LINUX_MUSLEABI_LINKER = "${bmmCc}/bin/armv5tel-unknown-linux-musleabi-gcc";
      # `secp256k1-sys` needs a cross compiler, as at `armTarget` in
      # units.nix, and it is the linker's own musleabi gcc here so the C
      # objects share the target ABI.
      CC_arm_unknown_linux_musleabi = "${bmmCc}/bin/armv5tel-unknown-linux-musleabi-gcc";
    }
    // webuiHostEnv
  );

  fmt = craneLib.cargoFmt {
    inherit src;
    doInstallCargoArtifacts = false;
  };

  # The per-unit plan of nix/units.nix leaves doc tests out, because
  # rustdoc compiles and runs them, so they build through crane in one
  # derivation. A crate that gains a runnable example joins the list.
  doctest-host =
    let
      args = {
        CARGO_PROFILE = "";
        # No default features, because the default `defmt` of ubos-metrics
        # reaches crates of this set that have no defmt of their own.
        cargoExtraArgs = "-p ubos-tuner -p ubos-events -p ubos-metrics --no-default-features --features ubos-events/std";
      };
    in
    craneLib.cargoDocTest (
      args
      // {
        src = srcFor [
          "ubos-tuner"
          "ubos-events"
          "ubos-metrics"
        ];
        # See `checkBag` in nix/deps.nix for why the overlay follows the
        # stubbing.
        cargoArtifacts = craneLib.buildDepsOnly (
          args
          // {
            inherit src;
            extraDummyScript = patchedCratesOverlay;
          }
        );
        doInstallCargoArtifacts = false;
      }
    );

  # Derived from the test table, so a new test row joins automatically.
  # The web/host-runner tests sit in web-host instead, because they need
  # the hermetic webui dist and rebuild whenever that build env changes.
  fast-host =
    let
      fastTests = builtins.removeAttrs hostTestChecks [
        "test-ubos-webui"
        "test-host-runner"
      ];
    in
    pkgs.runCommand "ubos-main-fast-host-tests"
      {
        nativeBuildInputs = builtins.attrValues fastTests;
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
          hostTestChecks.test-ubos-webui
          hostTestChecks.test-host-runner
        ];
      }
      ''
        mkdir -p "$out"
        printf '%s\n' \
          "ubos-main web/host-runner test aggregate" \
          "" \
          "Building this check forces the web UI and native host-runner tests to build/run." \
          "These checks require the hermetic webui dist dependency." \
          > "$out/README"
      '';

  # Derived from the board table plus the hand-written build checks
  # above, so a new board joins automatically.
  firmware-builds =
    pkgs.runCommand "ubos-main-firmware-builds"
      {
        nativeBuildInputs =
          builtins.attrValues (lib.filterAttrs (name: _: lib.hasPrefix "build-" name) boardChecks)
          ++ lib.optional (hasCrate "esp32s3") espChecks.build-esp32s3
          ++ lib.optional (hasCrate "rp2350-bootloader") rp2350BootloaderChecks.build-rp2350-bootloader
          ++ lib.optional (hasCrate "rp2040-bootloader") rp2040BootloaderChecks.build-rp2040-bootloader
          ++ lib.optional (hasCrate "esp32s3") espChecks.build-esp32s3-bootloader;
      }
      ''
        mkdir -p "$out"
        printf '%s\n' \
          "ubos-main firmware build aggregate" \
          "" \
          "Building this check forces all firmware release build checks to build." \
          "It covers every board's default-feature release build." \
          > "$out/README"
      '';

  # Fail the build on an unused dependency so dead deps cannot
  # accumulate. A dependency no parser can see is exempted per crate in
  # [package.metadata.cargo-shear] with its reason beside it, and a bare
  # `defmt` entry stands for the `ubos_logging` macros, which expand to
  # `defmt::` paths at the call site.
  cargo-shear = craneLib.mkCargoDerivation {
    inherit src;
    pnameSuffix = "-shear";
    cargoArtifacts = null;
    doInstallCargoArtifacts = false;
    nativeBuildInputs = [ pkgs.cargo-shear ];
    buildPhaseCargoCommand = "cargo shear --deny-warnings --offline";
  };

  # The root workspace excludes micro-sv2, so the run above reads none
  # of its manifests and this one vendors that workspace's own lockfile.
  cargo-shear-micro-sv2 = craneLib.mkCargoDerivation {
    src = lib.cleanSourceWith {
      src = root + "/micro-sv2";
      filter = craneLib.filterCargoSources;
      name = "source";
    };
    pname = "micro-sv2";
    version = "0";
    pnameSuffix = "-shear";
    cargoArtifacts = null;
    doInstallCargoArtifacts = false;
    nativeBuildInputs = [ pkgs.cargo-shear ];
    buildPhaseCargoCommand = "cargo shear --deny-warnings --offline";
  };

  # Holds the crate graph to the licences, the duplicate versions and
  # the sources deny.toml allows. Crane vendors the graph from the
  # lockfile, so cargo resolves it with no network. A warning fails the
  # run, because a skip or a licence that deny.toml lists and the graph
  # has dropped is only a warning.
  cargo-deny = craneLib.cargoDeny {
    inherit src;
    cargoDenyChecks = "-D warnings bans licenses sources";
  };

  # The crate graph against the layer table; see nix/layers.nix.
  layers = (import ./layers.nix { inherit pkgs; }).check { inherit root; };

  # nix/webui.nix admits only the files it lists into the dashboard
  # build, and build.rs lists the same files as its rebuild triggers.
  # Neither can read the other, and a file added to build.rs alone is
  # dropped from the hermetic dist without a word.
  webui-inputs = pkgs.runCommand "webui-inputs" { } ''
    sed -n '/for path in \[/,/\]/p' ${root + "/ubos/ubos-webui/build.rs"} \
      | grep -o '"[^"]*"' | tr -d '"' | sort > from-build-rs
    printf '%s\n' ${lib.escapeShellArgs webuiDist.inputs} | sort > from-nix
    if ! diff from-nix from-build-rs; then
      echo "webui input drift: nix/webui.nix (<) and build.rs (>) list different files" >&2
      exit 1
    fi
    touch $out
  '';

  # The OTA image-size limit is written in several languages that cannot
  # share a definition, so each source is grepped for the literal it must
  # contain. That is what keeps the slot-size build guards, the per-board
  # configs and the client-side rejection on one number per board.
  ota-image-limit = pkgs.runCommand "ota-image-limit" { } (
    ''
      check() {
        if ! grep -qF "$2" "$1"; then
          echo "OTA image limit drift: $1 lacks '$2'" >&2
          exit 1
        fi
      }
      # The shared RP ACTIVE-slot budget. These two copies are always
      # present; a per-board config is checked only when the board is a
      # member.
      check ${root + "/ubos/ubos-api/src/system.rs"}     '${toString slotMaxKiB} * 1024'
      check ${
        root + "/ubos/ubos-webui/web/components/ConfigSystem.tsx"
      }    '${toString slotMaxKiB} * 1024'
    ''
    + lib.optionalString (hasCrate "rp2040") "  check ${
        root + "/firmware/rp2040/src/config.rs"
      }      '${toString slotMaxKiB} * 1024'\n"
    + lib.optionalString (hasCrate "rp2350") "  check ${
        root + "/firmware/rp2350/src/config.rs"
      }      '${toString slotMaxKiB} * 1024'\n"
    + lib.optionalString (hasCrate "bmm-pico") "  check ${
        root + "/firmware/bmm-pico/src/config.rs"
      }    '${toString slotMaxKiB} * 1024'\n"
    + lib.optionalString (hasCrate "hashnode") "  check ${
        root + "/firmware/hashnode/src/config.rs"
      }    '${toString slotMaxKiB} * 1024'\n"
    + lib.optionalString (hasCrate "forge-axe") "  check ${
        root + "/firmware/forge-axe/src/config.rs"
      } '${toString slotMaxKiB} * 1024'\n"
    # esp32s3: the 3 MiB app slot, not the shared RP budget.
    + lib.optionalString (hasCrate "esp32s3") "  check ${
        root + "/firmware/esp32s3/src/config.rs"
      }     '3 * 1024 * 1024'\n"
    + ''
      touch $out
    ''
  );

  # The gate behind .claude/rules/licence-headers.md, which is where the
  # rule, the two halves of the block, and the reason the extension list
  # is the whole scope are written. A first-party source missing either
  # half fails here.
  license-headers =
    let
      extensions = [
        "c"
        "css"
        "html"
        "js"
        "nix"
        "py"
        "rs"
        "sh"
      ];
      # An agent hook is bash with no suffix, because the tool that runs
      # it names the file and never the language, so the scope reaches it
      # by path instead of by extension.
      paths = [ "./.claude/hooks/*" ];
      # The block is 18 lines. The rest of the budget covers a shebang or
      # an attribute above it on a file that grows one.
      headLines = 25;
      grant = "GNU General Public License as published by";
      reservation = "reserves the right to grant any party a license";
      names =
        lib.concatMapStringsSep " -o " (e: "-name '*.${e}'") extensions
        + lib.concatMapStrings (p: " -o -path '${p}'") paths;
    in
    pkgs.runCommand "license-headers" { } ''
      # The source is read-only, and a file is what carries the loop's
      # findings past the pipeline's subshell.
      report=$TMPDIR/missing-headers
      : > "$report"
      cd ${root}
      find . -type f \( ${names} \) -not -path './patches/*' \
        | sort \
        | while read -r f; do
            # Strip the comment leader of every language in the list,
            # then join the lines, so one grep answers for all of them.
            top=$(head -n ${toString headLines} "$f" \
              | sed 's|^[[:space:]]*\(//\|#\|/\*\|\*\|<!--\|-->\)*[[:space:]]*||' \
              | tr '\n' ' ')
            note=
            printf '%s' "$top" | grep -qF ${lib.escapeShellArg grant} \
              || note="no grant"
            printf '%s' "$top" | grep -qF ${lib.escapeShellArg reservation} \
              || note="''${note:+$note, }no reservation"
            if [ -n "$note" ]; then
              echo "  $f ($note)" >> "$report"
            fi
          done
      if [ -s "$report" ]; then
        echo "These first-party sources do not carry the full GPL block:" >&2
        cat "$report" >&2
        echo "" >&2
        echo "Copy the block from a neighbouring file in the same" >&2
        echo "language. The block has two parts, and this gate needs" >&2
        echo "both. The first part grants the GPL-3.0-or-later terms." >&2
        echo "The second part reserves the right of Braiins Systems" >&2
        echo "s.r.o. to grant a licence under other terms." >&2
        exit 1
      fi
      touch $out
    '';

  # Every board must take its task shells and its boot sequence from
  # ubos-firmware. nix/board-parity.nix holds the names both trees of the
  # pair share, so this call site names only this tree's exceptions: the
  # bootloaders, which run no embassy executor and have no boot sequence
  # to share.
  board-skeleton-parity = boardParity.boardSkeletonParity {
    inherit root;
    exclude = [
      "esp32s3-bootloader"
      "rp2040-bootloader"
      "rp2350-bootloader"
    ];
    holders = {
      forge-axe = [ "rgb_task" ];
    };
  };

  # Every board keeps the metrics store by default; see
  # `boardMetricsFeature` in nix/board-parity.nix. The bootloader crates
  # push no metric and have no feature to hold. The rp2040 and the esp32s3
  # ship without the store, because with it the stack left in RAM is
  # smaller than the stack the board needs at boot, and the overflow
  # faults the board.
  board-metrics-feature = boardParity.boardMetricsFeature {
    inherit root;
    exclude = [
      "esp32s3"
      "esp32s3-bootloader"
      "rp2040"
      "rp2040-bootloader"
      "rp2350-bootloader"
    ];
  };

  # The ESP bootloader is the one crate with no cargo config, because it
  # builds through the esp shell's own configuration rather than a runner
  # of its own.
  firmware-cargo-config = boardParity.firmwareCargoConfig {
    inherit root;
    exclude = [ "esp32s3-bootloader" ];
  };

  # Only ubos-metrics may call the metric and the name constructors,
  # because every other crate builds a metric through the macro that
  # checks its name.
  # nix/board-parity.nix holds the gate, and both trees of the pair run
  # it.
  metric-constructor = boardParity.metricConstructor {
    inherit root;
    excludeDirs = [ "ubos-metrics" ];
  };

  # The hook that holds `nix run .#gate` to its approved invocations runs
  # in the session's own shell, so it is loose text rather than a
  # package, and a lost executable bit would retire it without a word.
  gate-hook =
    let
      hookPath = ".claude/hooks/gate-invocation";
    in
    pkgs.runCommand "gate-hook"
      {
        nativeBuildInputs = [ pkgs.shellcheck ];
      }
      ''
        hook=${root + "/${hookPath}"}
        shellcheck "$hook"
        if [ ! -x "$hook" ]; then
          echo "ERROR: ${hookPath} is not executable." >&2
          echo "A hook without that bit never runs and reports nothing." >&2
          exit 1
        fi
        touch $out
      '';

  # `nix flake check` reads an app's schema and never builds its
  # program, so the shellcheck that `writeShellApplication` runs would
  # only ever run when somebody used the gate. Naming the program here
  # is what builds it.
  gate-app =
    let
      gate = import ./gate.nix {
        inherit pkgs;
        inherit (pkgs.stdenv.hostPlatform) system;
      };
    in
    pkgs.runCommand "gate-app" { } ''
      test -x ${gate.program}
      touch $out
    '';

  # The commit linter's script, built here for the same reason the
  # gate's is: naming the program is what runs its shellcheck.
  gitlint-app =
    let
      gitlint = import ./gitlint.nix { inherit pkgs; };
    in
    pkgs.runCommand "gitlint-app" { } ''
      test -x ${gitlint.program}
      touch $out
    '';

  # CLAUDE.md loads into context at the start of every session, and an
  # agent's adherence to it drops as it grows. The budget is CLAUDE.md's
  # own, gated here rather than trusted to the next edit.
  claude-md-limit = pkgs.runCommand "claude-md-limit" { } ''
    lines=$(wc -l < ${root + "/CLAUDE.md"})
    echo "CLAUDE.md: $lines lines / ${toString claudeMdMaxLines}"
    if [ "$lines" -gt ${toString claudeMdMaxLines} ]; then
      echo "ERROR: CLAUDE.md is $lines lines; the limit is ${toString claudeMdMaxLines}." >&2
      echo "Move detail into docs/ or .claude/rules/ instead of growing it." >&2
      exit 1
    fi
    touch $out
  '';

  # The base crates list only the workspace crates their rows allow.
  # The rows and the reason are in nix/crate-layers.nix.
  crate-layers = import ./crate-layers.nix { inherit pkgs crateGraph; };

  # The paths instruction files name; the rule and the reason are in
  # nix/instruction-paths.nix, which both trees of the pair call.
  instruction-paths = import ./instruction-paths.nix {
    inherit pkgs root;
    minFiles = minInstructionFiles;
  };

  # The rule, and why it is shared with the internal repository, are in
  # nix/brand-gates.nix. This call supplies the two things that module
  # cannot know: which gates this tree has and which crates it builds.
  board-coverage = brandGates.boardCoverage {
    boards = builtins.attrNames boardTable ++ handGatedBoards;
    isMember = hasCrate;
  };

  # nix/toolchains.nix names the Rust version once and every toolchain
  # there reads that name. Two files cannot reach it and must state it
  # again, so each is grepped for the literal as in ota-image-limit:
  # micro-sv2 is a flake of its own, and a flake reads no path outside
  # its root, and stratum-v1's rust-toolchain.toml is rustup's format
  # rather than nix's.
  rust-version-agreement = pkgs.runCommand "rust-version-agreement" { } ''
    check() {
      if ! grep -qF "$2" "$1"; then
        echo "The Rust version is not the same in all files." >&2
        echo "nix/toolchains.nix names ${rustVersion}." >&2
        echo "This file does not have '$2':" >&2
        echo "  $1" >&2
        exit 1
      fi
    }
    check ${root + "/micro-sv2/flake.nix"} 'rustVersion = "${rustVersion}";'
    check ${root + "/ubos/stratum-v1/rust-toolchain.toml"} 'channel = "${rustVersion}"'
    touch $out
  '';

  # The reason the brand a board ships is stated twice, and the gate
  # itself, are in nix/brand-gates.nix, which both trees of the pair
  # call. The list below is the one question that module cannot answer
  # for itself: which boards declare a brand.
  brand-declaration = brandGates.brandDeclaration {
    boards = builtins.attrNames boards ++ lib.optional (hasCrate "esp32s3") "esp32s3";
  };

  # The flake gates its own .nix files, so `nix flake check` stays the
  # single source of truth for format in a standalone repo.
  nixfmt =
    pkgs.runCommand "nixfmt-check"
      {
        nativeBuildInputs = [ pkgs.nixfmt ];
      }
      ''
        find ${root} -name '*.nix' -print0 | xargs -0 nixfmt --check
        touch $out
      '';

  # The writeShellApplication and writePython3Bin wrappers under pkgs/
  # run shellcheck or pyflakes on their source at build time, so naming
  # each package here is its own lint gate and no tree-wide shell sweep
  # is needed.
  probe-rs-board-run = pkgs.callPackage ../pkgs/probe-rs-board-runner { };
  rp2040-bootloader-run = pkgs.callPackage ../pkgs/rp2040-bootloader-runner { };
  rp2350-bootloader-run = pkgs.callPackage ../pkgs/rp2350-bootloader-runner { };
  setup-network = pkgs.callPackage ../pkgs/setup-network { };
  host-runner-run = pkgs.callPackage ../pkgs/host-runner-run { };
  elf-symbol = elfSymbol;
}
