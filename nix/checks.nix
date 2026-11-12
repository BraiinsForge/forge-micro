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

# Per-crate gates and static guards. Every crate in the workspace is
# covered by a clippy and a build or test derivation here; a crate
# that is not in `checks` is not gated, per the Verification section
# of CLAUDE.md.
#
# The regular shapes are generated from the tables below, so gating a
# new host crate is one line in `hostClippyChecks`, plus
# `hostTestChecks` when it has unit tests, and a new board is one entry
# in `boards`. The `ubos-config`/`ubos-events` std feature variants and
# the host build of `host-runner` go through those tables too. The
# irregular checks stay hand-written because forcing them through the
# tables would need a knob per quirk: esp32s3, the bootloaders, and the
# bmm cross build of `host-runner`.
#
# The `[patch]` override crates, materialized from upstream pins plus
# local patch files by nix/patched-crates.nix, are intentionally not
# gated by their own clippy or build derivations. They are upstream
# crates carrying local fixes that we do not author; gating them in CI
# would tie the build to upstream lint cleanliness we do not control.
# Every firmware build derivation below exercises them transitively,
# so a real breakage still fails the gate.
{
  pkgs,
  src,
  srcFor,
  deps,
  craneLib,
  # Every cargo unit as one derivation; see nix/units.nix. The host
  # checks, the ARM checks and the images build through it.
  units,
  # The build-script environment of ubos-webui, per board and for a
  # host build; see flake.nix.
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
  # The one rust version, from nix/toolchains.nix, for the drift gate
  # over micro-sv2, which cannot import it.
  rustVersion,
  slotMaxKiB,
  slotMaxBytes,
  esp32s3SlotBytes,
  boardParity,
  # Which brand each board ships, from nix/brands.nix. Every build that
  # compiles ubos-webui takes UBOS_BRAND from it.
  brands,
  # The brand gate shared with the internal repository; see
  # nix/brand-gates.nix.
  brandGates,
  # Workspace package names present in the tree, from crate-graph. Every
  # firmware gate below filters itself out when its crate is not a
  # member, so a tree carrying a subset of the boards evaluates with
  # those gates absent instead of dangling on sources it does not have.
  # In this repo every board is a member, so the filters are the identity
  # and gate all of them.
  members,
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

  # Line budget for CLAUDE.md; see claude-md-limit below.
  claudeMdMaxLines = 200;

  # The fewest instruction files instruction-paths below expects to
  # reach. It exists to catch a scan that reads nothing, not to fix the
  # count, so it sits far under what the tree holds and moves only if
  # the tree ever holds fewer.
  minInstructionFiles = 5;

  # The fee switch, from nix/fee.nix. The host gates run with it on:
  # that is the wider state, the one the fee-slot tests and the config
  # body pin in ubos-api measure. A row that states `fee = fee.off`
  # gates the other state. A board gate takes the state its image
  # ships, read from the board's own cargo config.
  fee = import ./fee.nix;
  feeEnv = name: fee.envFor (root + "/firmware/${name}");

  # A firmware crate is gated only when it is a workspace member. In this
  # repo every board is a member, so this is always true; it is what lets
  # the module evaluate against a tree that carries fewer. See the
  # `members` arg.
  hasCrate = name: builtins.elem name members;

  # Host-side clippy: one row per check, one plan per row, as `cargo
  # clippy -p` planned it. `crate` is the cargo package; `features` and
  # `noDefaultFeatures` are its command line; `tests` is off for crates
  # whose tests do not build on the host feature set, which is what
  # `--all-targets` was left off for, and for crates whose tests link
  # only under the features their test row names, which `cargo clippy`
  # never linked. `fee` is the fee switch, on unless the row says
  # `fee.off`. The check holds the crate's units
  # of the plan: its library, binaries and, with `tests`, test
  # executables. No gate takes the build id: it changes with every
  # commit, and a gate stamped with it would rebuild on every commit for
  # an id nothing reads. Only the images in nix/images.nix carry it.
  mkHostClippy =
    name: row:
    pkgs.linkFarmFromDrvs name (units.unitsOf (units.hostPlan ({ fee = fee.on; } // row)) row.crate);

  # Host-side unit tests: the crate's test runs of its plan, in the dev
  # profile so debug_assert! and overflow-checks fire in CI.
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

  hostTestChecks = builtins.mapAttrs mkHostTest {
    test-ubos-core.crate = "ubos-core";
    test-ubos-mdns.crate = "ubos-mdns";
    test-ubos-net.crate = "ubos-net";
    test-ubos-psu.crate = "ubos-psu";
    test-ubos-snapshot.crate = "ubos-snapshot";
    test-ubos-eeprom.crate = "ubos-eeprom";
    test-embedded-i2c-unix.crate = "embedded-i2c-unix";
    test-ubos-auth.crate = "ubos-auth";
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

  # One row per ARM board. Generates clippy-<name> and build-<name>,
  # both with the default feature set, and the build runs the image
  # guard over the linked ELF.
  #
  # `boardTable` is this table before `hasCrate` filters it, and
  # `board-coverage` below reads that unfiltered form. The filter is
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
  # hand-written check above. `board-coverage` needs them because it
  # asks whether a firmware directory is gated at all, not whether it
  # goes through the table.
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

  # The image guard, a check over the ELF a board plan links. It
  # objcopies the ELF and enforces the OTA slot budget. cargo check and
  # cargo clippy stop short of linking the final ELF for binary crates,
  # so they miss .bss / flash overflows; the linked image is what fails
  # CI on a RAM or flash budget regression, at the same gate as a
  # missing trait bound. The Footprint section of docs/STRUCTURE.md
  # records the per-board RAM snapshot this gate protects; docs/OTA.md
  # the flash layout and slot budget. The guard text renders from
  # nix/guards.nix. The guard also reads the config schema hash out of
  # the ELF, so an image whose link lost the symbol fails its build
  # check rather than the packing of an update.
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

  # A board's clippy and build checks in one fee state, under names
  # that end in `suffix`. The plan is one graph, so the build is its
  # image guarded, and clippy the same plan.
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

  # Each board in the fee state its image ships.
  boardChecks = lib.concatMapAttrs (name: mkBoardChecks "" (feeEnv name) name) boards;

  # The other state of the fee switch, one board per ARM target that
  # can carry the fee, so the on path of the board crates cannot rot
  # unseen in a tree whose images ship it off. The build row is what
  # measures the RAM a fourth session task costs. The Xtensa rows are
  # in `espChecks`.
  feeOnBoards = [
    "rp2040"
    "rp2350"
  ];
  feeOnChecks = lib.concatMapAttrs (mkBoardChecks "-on" fee.on) (
    lib.filterAttrs (name: _: lib.elem name feeOnBoards) boards
  );

  # A library crate's board check: the crate's units of its plan. The
  # plan's own result holds only what it installs, which for a library
  # is nothing, so a check that took it would build none of the crate.
  mkLibraryCheck =
    name: args: pkgs.linkFarmFromDrvs name (units.unitsOf (units.boardPlan args) args.name);

  # Bootloader crates: the same clippy/build pair as the boards, with
  # no image guard, since the bootloader carries no pool worker and has
  # its own flash region.
  mkBootloaderChecks =
    name: target:
    let
      plan = units.boardPlan { inherit name target; };
    in
    {
      "clippy-${name}" = plan;
      "build-${name}" = plan;
    };

  # Common attrs for every xtensa esp32s3 crane invocation: the shared
  # cross-build env from helpers.espCrossEnv, plus the esp32s3 source,
  # the vendored build-std deps, and the toolchain build inputs.
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

  # esp32s3 firmware gates, grouped so they can be dropped as a unit when
  # the board is not a member. They share the esp toolchain and the
  # esp32s3 source scaffolding, so the esp backend of ubos-ota and the
  # esp32s3-tsens lib are gated here too: a tree without the board pulls
  # in no Xtensa toolchain at all.
  # No codegen bag to reuse; see the esp bag rationale in nix/deps.nix.
  # The size guard must live here, not only in packages.esp32s3-image:
  # `nix flake check` builds checks.* but merely evaluates packages.*,
  # so a guard in the image package alone would never gate an oversized
  # image in CI.
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

    # The release build in the fee state the image ships.
    build-esp32s3 = espBuildIn (feeEnv "esp32s3");

    # The other fee state on Xtensa, built; see `feeOnChecks`.
    build-esp32s3-on = espBuildIn fee.on;

    # Gate the OTA bootloader so a broken sdkconfig fails CI. This
    # pulls the esp-idf closure on first build; it is cached until the
    # nixpkgs-esp-dev input or the stub project changes.
    build-esp32s3-bootloader = esp32s3Bootloader;

    # Lint the ubos-ota esp backend. clippy only lints the top crate,
    # so the esp32s3 firmware build compiles this code but does not
    # lint it; this check does. The chip features are supplied on the
    # CLI, matching clippy-ubos-ota for the rp backend.
    clippy-ubos-ota-esp = espCraneLib.cargoClippy (
      espCommon
      // {
        src = srcFor [ "ubos-ota" ];
        cargoArtifacts = deps.espArtifacts;
        cargoExtraArgs =
          "-p ubos-ota --target xtensa-esp32s3-none-elf -Z build-std=core "
          + "--features esp,esp-hal/esp32s3,esp-hal/unstable,esp-storage/esp32s3,"
          + "esp-storage/critical-section,esp-bootloader-esp-idf/esp32s3";
        cargoClippyExtraArgs = "-- -D warnings";
      }
      // webuiHostEnv
    );

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

  clippy-ubos-ota = mkLibraryCheck "clippy-ubos-ota" {
    name = "ubos-ota";
    target = "thumbv8m.main-none-eabihf";
    features = [
      "rp"
      "embassy-rp/rp235xa"
      "embassy-rp/imagedef-none"
    ];
  };

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
      # ubos-webui's build.rs runs as part of host-runner's dependency
      # closure, so it needs the hermetic dist too.
      nativeBuildInputs = [
        bmmCc
      ];
      CARGO_TARGET_ARM_UNKNOWN_LINUX_MUSLEABI_LINKER = "${bmmCc}/bin/armv5tel-unknown-linux-musleabi-gcc";
      # SV2 pulls in `secp256k1-sys`, whose C build script defers to
      # `cc-rs`. Point it at the same musleabi cross-gcc the linker
      # uses so the C objects share the target ABI.
      CC_arm_unknown_linux_musleabi = "${bmmCc}/bin/armv5tel-unknown-linux-musleabi-gcc";
    }
    // webuiHostEnv
  );

  fmt = craneLib.cargoFmt {
    inherit src;
    doInstallCargoArtifacts = false;
  };

  # The doc tests of every crate whose docs hold a runnable example, in
  # the dev profile. The per-unit plan of nix/units.nix leaves doc tests
  # out, because rustdoc compiles and runs them, so they build through
  # crane in one derivation, and a crate that gains a runnable example
  # joins the list.
  doctest-host =
    let
      args = {
        CARGO_PROFILE = "";
        cargoExtraArgs = "-p ubos-tuner -p ubos-events --features ubos-events/std";
      };
    in
    craneLib.cargoDocTest (
      args
      // {
        src = srcFor [
          "ubos-tuner"
          "ubos-events"
        ];
        cargoArtifacts = craneLib.buildDepsOnly (args // { inherit src; });
        doInstallCargoArtifacts = false;
      }
    );

  # Aggregate over every host-testable crate check, so one build forces
  # them all; derived from the test table, a new test row joins
  # automatically. The web/host-runner tests sit in web-host instead:
  # they additionally need the hermetic webui dist and rebuild whenever
  # the webui build env changes.
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

  # Aggregate over every firmware release build; derived from the board
  # table plus the hand-written build checks above.
  firmware-builds =
    pkgs.runCommand "ubos-main-firmware-builds"
      {
        # Board release builds present in this tree, plus the esp and
        # bootloader builds when their crates are members.
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
        # Scan a copy with patches/ removed: those are pinned-and-patched
        # upstream trees (nix/patched-crates.nix), not crates this repo
        # owns, and machete's static import scan misfires on their
        # generated/macro-only code (e.g. an svd2rust PAC build script, a
        # bitflags! invocation) in a way we cannot fix without patching
        # vendored manifests beyond the one change each patch file
        # already documents. Excluding the directory keeps the gate
        # scoped to what this repo owns.
        cp -a ${src} ./scan-src
        chmod -R u+w ./scan-src
        rm -rf ./scan-src/patches
        cargo-machete ./scan-src
        touch $out
      '';

  # nix/webui.nix admits only the files it lists into the dashboard
  # build, and build.rs lists the same files as its rebuild triggers.
  # Neither can read the other, and a file added to build.rs alone is
  # dropped from the hermetic dist without a word, so the two lists are
  # compared here and any difference fails.
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

  # The OTA image-size limit is hard-coded in several languages that
  # cannot share a definition: slotMaxBytes/esp32s3SlotBytes here,
  # the OTA_MAX_IMAGE_BYTES default in ubos-api, OTA_MAX_DEFAULT in the
  # webui, and Ota::MAX_IMAGE_BYTES in each board's config.rs. Grep
  # each source for the literal it must contain and fail on drift, so
  # the slot-size build guards, the per-board configs, and the
  # client-side rejection always agree on one number per board.
  ota-image-limit = pkgs.runCommand "ota-image-limit" { } (
    ''
      check() {
        if ! grep -qF "$2" "$1"; then
          echo "OTA image limit drift: $1 lacks '$2'" >&2
          exit 1
        fi
      }
      # Shared RP ACTIVE-slot budget: the ubos-api default, the web-UI
      # fallback, and the RP boards all pin slotMaxKiB * 1024. The
      # ubos-api and web-UI copies are always present; the per-board
      # config checks are gated on the board being a member.
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

  # The converse of the license-headers gate in the private repo that
  # consumes this one. That gate fails when an open grant reaches a tree
  # publishing nothing; this one fails when a first-party source here
  # does not carry the GPL block that .claude/rules/licence-headers.md
  # requires. The rule was prose alone until this gate, so nothing but
  # the next reader stopped a file from omitting the block.
  #
  # Both halves are required. The grant is the first, and the sentence
  # reserving the right to grant other terms is the second. The
  # proprietary crates in the consuming repo link these sources on that
  # reservation, so the grant alone is a gap rather than a formatting
  # slip.
  #
  # The extension list is the scope, and it holds the exceptions named in
  # CLAUDE.md out of it without an exemption list to keep level with
  # them: assets/69-probe-rs.rules and ubos/ubos-webui/web/icons.svg
  # are under another licence, COPYING and assets/LICENSE-PROBE-RS are
  # licence texts, and none of the four has a gated extension. Vendored
  # patches/ trees keep upstream's licence and are dropped by path.
  #
  # Unlike its private twin, this check needs no split string literals.
  # That one fails on a file carrying the phrases, so writing them in its
  # own source made it fail on itself. This one fails on a file lacking
  # them, and its own source carries the block, so it passes.
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
      # A first-party source the extension list cannot name. An agent
      # hook is bash with no suffix, because the tool that runs it names
      # the file and never the language, so the scope has to reach it by
      # path instead. Without this the one first-party source in the
      # tree that carries no extension was ungated.
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
      # The report lands outside the source, which is read-only, and it
      # carries the loop's findings past the pipeline's subshell.
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
  # pair share; this call site names only this tree's exceptions, and the
  # board list comes from firmware/ itself so a new board is gated the
  # day it lands. The three bootloader crates are excluded: they run no
  # embassy executor and have no boot sequence to share.
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

  # The ESP bootloader is the one crate with no cargo config, because it
  # builds through the esp shell's own configuration rather than a
  # runner of its own.
  firmware-cargo-config = boardParity.firmwareCargoConfig {
    inherit root;
    exclude = [ "esp32s3-bootloader" ];
  };

  # The hook that holds `nix run .#gate` to its approved invocations. It
  # runs in the session's own shell, so it is loose text rather than a
  # package, and a lost executable bit would retire it without a word.
  # The regex is its whole content, so shellcheck is the only test it
  # can carry here.
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

  # The gate's own script, which nothing else here builds. `nix flake
  # check` reads an app's schema and never builds its program, so the
  # shellcheck that `writeShellApplication` runs would only ever have
  # run when somebody used the gate. Naming the program is what builds
  # it, the same way `test -x` does for the apps of the repo that
  # consumes this one.
  #
  # The image apps are deliberately not covered: each interpolates the
  # firmware derivation it copies, so naming them would build every
  # image again under a second name. The checks that build those images
  # already exist.
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

  # CLAUDE.md is loaded into context at the start of every session, and
  # an agent's adherence to it drops as it grows: past a few hundred
  # lines the rules that matter get lost among the ones that do not.
  # claudeMdMaxLines is the documented target, so gate it rather than
  # trusting the next edit to notice. Detail belongs in docs/ or, when
  # it is only relevant under one tree, in a path-scoped .claude/rules/
  # file, which loads on demand instead.
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

  # The paths instruction files name; the rule and the reason are in
  # nix/instruction-paths.nix, which both trees of the pair call.
  instruction-paths = import ./instruction-paths.nix {
    inherit pkgs root;
    minFiles = minInstructionFiles;
  };

  # Every firmware crate is named by a gate, and every gate names a
  # firmware crate. The rule, and why it is one shared with the internal
  # repository, are in nix/brand-gates.nix; this tree supplies the two
  # things that module cannot know, which gates it has and which crates
  # it builds.
  board-coverage = brandGates.boardCoverage {
    boards = builtins.attrNames boardTable ++ handGatedBoards;
    isMember = hasCrate;
  };

  # nix/toolchains.nix names the Rust version once and every toolchain
  # there reads that name. Two files cannot reach it and must state it
  # again: micro-sv2 is a flake of its own, and a flake reads no path
  # outside its root, and stratum-v1's rust-toolchain.toml is rustup's
  # format rather than nix's. Grep each for the literal and fail on
  # drift, the same method ota-image-limit uses. The flake above
  # re-exports the micro-sv2 checks, so a drift there is a second
  # compiler in one gate set. A drift in stratum-v1 reaches no build
  # this repo sanctions, because each of those goes through a
  # `nix develop` shell where cargo is nix's and reads no rustup file.
  # It reaches rust-analyzer, and anyone who builds that crate through
  # rustup outside a shell, and it is worth gating for them.
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

  # The brand a board ships is stated twice, and it has to be; the
  # reason, and the gate itself, are in nix/brand-gates.nix, which both
  # trees of the pair call. The list below is the one question that
  # module cannot answer for itself: which boards declare a brand. The
  # `boards` table covers the ARM and STM ones; esp32s3 is hand-written
  # above and joins here.
  brand-declaration = brandGates.brandDeclaration {
    boards = builtins.attrNames boards ++ lib.optional (hasCrate "esp32s3") "esp32s3";
  };

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
        find ${root} -name '*.nix' -print0 | xargs -0 nixfmt --check
        touch $out
      '';

  # Script packages: the writeShellApplication/writePython3Bin wrappers
  # under pkgs/ run shellcheck / pyflakes on their source at build time,
  # so building each one is its own lint gate — no tree-wide shell sweep
  # is needed, and a new package is gated the moment it is wired here.
  probe-rs-board-run = pkgs.callPackage ../pkgs/probe-rs-board-runner { };
  rp2040-bootloader-run = pkgs.callPackage ../pkgs/rp2040-bootloader-runner { };
  rp2350-bootloader-run = pkgs.callPackage ../pkgs/rp2350-bootloader-runner { };
  setup-network = pkgs.callPackage ../pkgs/setup-network { };
  host-runner-run = pkgs.callPackage ../pkgs/host-runner-run { };
  elf-symbol = elfSymbol;
}
