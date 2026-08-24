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
  bmmCraneLib,
  espCraneLib,
  bmmCc,
  espToolchain,
  xtensaEspToolchain,
  espCargoVendorDir,
  esp32s3Bootloader,
  guards,
  helpers,
  slotMaxKiB,
  slotMaxBytes,
  esp32s3SlotBytes,
  boardParity,
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
    armCc
    webuiEnvAttrs
    webuiDistEnv
    espCrossEnv
    espRustflags
    ;
  root = ../.;

  # Line budget for CLAUDE.md; see claude-md-limit below.
  claudeMdMaxLines = 200;

  # A firmware crate is gated only when it is a workspace member. In this
  # repo every board is a member, so this is always true; it is what lets
  # the module evaluate against a tree that carries fewer. See the
  # `members` arg.
  hasCrate = name: builtins.elem name members;

  # Host-side clippy: one row per check. `crate` is the cargo package;
  # `artifacts` defaults to the shared host bag; `allTargets` is off for
  # crates whose tests or examples do not build on the host feature set;
  # `args` appends feature flags; `webuiEnv` and `webuiDist` cover
  # ubos-webui's build.rs, the latter pointing it at the hermetic dist.
  mkHostClippy =
    spec:
    craneLib.cargoClippy (
      {
        src = srcFor [ spec.crate ];
        cargoArtifacts = spec.artifacts or deps.hostArtifacts;
        cargoExtraArgs = "-p ${spec.crate}${spec.args or ""}";
        cargoClippyExtraArgs = "${
          lib.optionalString (spec.allTargets or true) "--all-targets "
        }-- -D warnings";
        doInstallCargoArtifacts = false;
      }
      // lib.optionalAttrs (spec.webuiEnv or false) webuiEnvAttrs
      // lib.optionalAttrs (spec.webuiDist or false) webuiDistEnv
    );

  # Host-side unit tests, dev profile so debug_assert! and
  # overflow-checks fire in CI.
  mkHostTest =
    spec:
    craneLib.cargoTest (
      {
        src = srcFor [ spec.crate ];
        cargoArtifacts = spec.artifacts or deps.hostArtifactsDev;
        CARGO_PROFILE = "";
        cargoExtraArgs = "-p ${spec.crate}${spec.args or ""}";
        doInstallCargoArtifacts = false;
      }
      // lib.optionalAttrs (spec.webuiEnv or false) webuiEnvAttrs
      // lib.optionalAttrs (spec.webuiDist or false) webuiDistEnv
    );

  hostClippyChecks = builtins.mapAttrs (_: mkHostClippy) {
    clippy-ubos-core.crate = "ubos-core";
    clippy-ubos-http.crate = "ubos-http";
    clippy-ubos-api.crate = "ubos-api";
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
    clippy-stratum-v1.crate = "stratum-v1";
    clippy-stratum-v2.crate = "stratum-v2";
    clippy-stratum-client.crate = "stratum-client";
    clippy-ubos-temperature.crate = "ubos-temperature";
    clippy-ubos-display = {
      crate = "ubos-display";
      allTargets = false;
    };
    clippy-ubos-logging = {
      crate = "ubos-logging";
      allTargets = false;
    };
    clippy-embassy-net-afpacket = {
      crate = "embassy-net-afpacket";
      allTargets = false;
    };
    clippy-embedded-serial-unix = {
      crate = "embedded-serial-unix";
      allTargets = false;
    };
    clippy-serial-baudrate = {
      crate = "serial-baudrate";
      allTargets = false;
    };
    clippy-ubos-machine = {
      crate = "ubos-machine";
      allTargets = false;
    };
    clippy-ubos-main-host = {
      crate = "ubos-main-host";
      allTargets = false;
    };
    clippy-ubos-webui = {
      crate = "ubos-webui";
      # build.rs reads the prebuilt Rsbuild pages from UBOS_WEBUI_DIST
      # and Brotli-compresses them, so the sandbox needs no npm.
      webuiDist = true;
      webuiEnv = true;
    };
    clippy-ubos-thermal = {
      crate = "ubos-thermal";
      artifacts = deps.ubosThermalArtifacts;
    };
    clippy-ubos-cooling = {
      crate = "ubos-cooling";
      artifacts = deps.ubosCoolingArtifacts;
    };
    clippy-ubos-manager = {
      crate = "ubos-manager";
      artifacts = deps.ubosManagerArtifacts;
    };
    clippy-ubos-config = {
      crate = "ubos-config";
      artifacts = deps.ubosConfigArtifacts;
    };
    clippy-ubos-config-std = {
      crate = "ubos-config";
      args = " --features std";
      artifacts = deps.ubosConfigStdArtifacts;
    };
    clippy-ubos-events = {
      crate = "ubos-events";
      allTargets = false;
      artifacts = deps.ubosEventsArtifacts;
    };
    clippy-ubos-events-std = {
      crate = "ubos-events";
      args = " --features std";
      artifacts = deps.ubosEventsStdArtifacts;
    };
    clippy-host-runner = {
      crate = "host-runner";
      artifacts = deps.hostRunnerArtifacts;
      # ubos-webui's build.rs runs as part of host-runner's dependency
      # closure, so it needs the hermetic dist too.
      webuiDist = true;
      webuiEnv = true;
    };
  };

  hostTestChecks = builtins.mapAttrs (_: mkHostTest) {
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
    test-stratum-client.crate = "stratum-client";
    test-mining-protocol.crate = "mining-protocol";
    test-ubos-asic.crate = "ubos-asic";
    test-ubos-machine.crate = "ubos-machine";
    test-ubos-temperature.crate = "ubos-temperature";
    test-ubos-http = {
      crate = "ubos-http";
      artifacts = deps.ubosHttpArtifactsDev;
    };
    # Host tests build with defmt off. ubos-api's default `defmt`
    # feature routes its `ubos_logging` calls through `defmt::*`
    # macros, which reference `_defmt_acquire`/`_defmt_timestamp` and
    # friends. A host test binary links no `defmt::global_logger`, so
    # those symbols are undefined and the test fails to link.
    test-ubos-api = {
      crate = "ubos-api";
      args = " --no-default-features";
      artifacts = deps.ubosApiArtifactsDev;
    };
    test-ubos-webui = {
      crate = "ubos-webui";
      # build.rs reads the prebuilt Rsbuild pages from UBOS_WEBUI_DIST
      # and Brotli-compresses them, so the sandbox needs no npm.
      webuiDist = true;
      webuiEnv = true;
    };
    test-host-runner = {
      crate = "host-runner";
      artifacts = deps.hostRunnerArtifactsDev;
      # ubos-webui's build.rs runs as part of host-runner's dependency
      # closure, so it needs the hermetic dist too.
      webuiDist = true;
      webuiEnv = true;
    };
    test-ubos-thermal = {
      crate = "ubos-thermal";
      artifacts = deps.ubosThermalArtifactsDev;
    };
    # Host tests build with defmt off; see the test-ubos-api comment
    # above. The staleness-failsafe test drives the real `run_cooling`
    # task, whose `ubos_logging` calls hit the same undefined
    # `_defmt_*` symbols once defmt is on.
    test-ubos-cooling = {
      crate = "ubos-cooling";
      args = " --no-default-features";
      artifacts = deps.ubosCoolingArtifactsDev;
    };
    test-ubos-manager = {
      crate = "ubos-manager";
      artifacts = deps.ubosManagerArtifactsDev;
    };
    test-ubos-config = {
      crate = "ubos-config";
      artifacts = deps.ubosConfigArtifactsDev;
    };
    test-ubos-config-std = {
      crate = "ubos-config";
      args = " --features std";
      artifacts = deps.ubosConfigStdArtifactsDev;
    };
    test-ubos-events = {
      crate = "ubos-events";
      args = " --features std";
      artifacts = deps.ubosEventsStdArtifactsDev;
    };
  };

  # One row per ARM/STM board. Generates clippy-<name> and
  # build-<name>, both with the default feature set. `guard`
  # selects the postBuild image guard: "slot" objcopies the ELF and
  # enforces the OTA slot budget, "elf" greps the ELF for the pool
  # worker only; boards that are not OTA targets have no slot budget.
  boards = lib.filterAttrs (name: _: hasCrate name) {
    braiins-axe = {
      artifacts = deps.thumbv8mArtifacts;
      target = "thumbv8m.main-none-eabihf";
      guard = "slot";
    };
    bmm-pico = {
      artifacts = deps.thumbv8mArtifacts;
      target = "thumbv8m.main-none-eabihf";
      guard = "slot";
    };
    rp2040 = {
      artifacts = deps.thumbv6mArtifacts;
      target = "thumbv6m-none-eabi";
      guard = "slot";
    };
    rp2350 = {
      artifacts = deps.thumbv8mArtifacts;
      target = "thumbv8m.main-none-eabihf";
      guard = "slot";
    };
    hashnode = {
      artifacts = deps.hashnodeArtifacts;
      target = "thumbv8m.main-none-eabihf";
      guard = "slot";
    };
    stm32f407 = {
      artifacts = deps.stm32f407Artifacts;
      target = "thumbv7em-none-eabihf";
      guard = "elf";
    };
    stm32f439 = {
      artifacts = deps.stm32f439Artifacts;
      target = "thumbv7em-none-eabihf";
      guard = "elf";
    };
  };

  # Slot-size guard plus pool-worker guard for OTA boards. See
  # docs/OTA.md for the flash layout and slot budget; the guard text
  # itself renders from nix/guards.nix.
  slotGuard =
    name: target:
    ''
      elf=target/${target}/release/${name}
      bin=$(mktemp --suffix=.bin)
      arm-none-eabi-objcopy -O binary "$elf" "$bin"
    ''
    + guards.imageGuard {
      inherit name;
      bin = ''"$bin"'';
      maxBytes = slotMaxBytes;
      cleanup = ''rm -f "$bin"; '';
    }
    + ''
      rm -f "$bin"
    '';

  # Pool-worker guard for boards that are not OTA targets: no slot
  # budget, so the guard greps the ELF directly rather than objcopying
  # a slot-sized binary.
  elfGuard =
    name: target:
    ''
      elf=target/${target}/release/${name}
    ''
    + guards.imageGuard {
      inherit name;
      bin = ''"$elf"'';
    };

  # `secp256k1-sys` is an unconditional dep via `ii-sv2` and its C
  # build script requires a cross compiler; `cc-rs` otherwise falls
  # back to host `gcc` and fails on the thumb-specific flags, so every
  # board check points it at arm-none-eabi-gcc. The hermetic webui
  # dist covers ubos-webui's build.rs in each board's dependency
  # closure.
  mkBoardClippy =
    name: board:
    craneLib.cargoClippy (
      {
        src = srcFor [ name ];
        cargoArtifacts = board.artifacts;
        cargoExtraArgs = "-p ${name} --target ${board.target}";
        cargoClippyExtraArgs = "-- -D warnings";
        doInstallCargoArtifacts = false;
        nativeBuildInputs = [
          pkgs.gcc-arm-embedded
        ];
      }
      // armCc board.target
      // webuiEnvAttrs
      // webuiDistEnv
    );

  # Release build of each firmware binary. cargo check and cargo
  # clippy stop short of linking the final ELF for binary crates, so
  # they miss .bss / flash overflows. This derivation invokes the full
  # linker pass so any RAM or flash budget regression fails CI at the
  # same gate as a missing trait bound. The Footprint section of
  # docs/STRUCTURE.md records the per-board RAM snapshot this gate
  # protects.
  mkBoardBuild =
    name: board:
    craneLib.cargoBuild (
      {
        src = srcFor [ name ];
        cargoArtifacts = board.artifacts;
        doInstallCargoArtifacts = false;
        cargoBuildCommand = "cargo build --release";
        cargoExtraArgs = "-p ${name} --target ${board.target}";
        nativeBuildInputs = [
          pkgs.gcc-arm-embedded
        ];
      }
      // armCc board.target
      // webuiEnvAttrs
      // webuiDistEnv
      // lib.optionalAttrs (board.guard == "slot") {
        postBuild = slotGuard name board.target;
      }
      // lib.optionalAttrs (board.guard == "elf") {
        postBuild = elfGuard name board.target;
      }
    );

  boardChecks = lib.concatMapAttrs (name: board: {
    "clippy-${name}" = mkBoardClippy name board;
    "build-${name}" = mkBoardBuild name board;
  }) boards;

  # Bootloader crates: same clippy/build pair as the boards, but no
  # webui env or dist, since ubos-webui is not in the closure, and
  # no image guard, since the bootloader carries no pool worker and
  # has its own flash region.
  mkBootloaderChecks =
    name: bag: target:
    let
      common = {
        src = srcFor [ name ];
        cargoArtifacts = bag;
        cargoExtraArgs = "-p ${name} --target ${target}";
        doInstallCargoArtifacts = false;
        nativeBuildInputs = [ pkgs.gcc-arm-embedded ];
      }
      // armCc target;
    in
    {
      "clippy-${name}" = craneLib.cargoClippy (
        common
        // {
          cargoClippyExtraArgs = "-- -D warnings";
        }
      );
      "build-${name}" = craneLib.cargoBuild (
        common
        // {
          cargoBuildCommand = "cargo build --release";
        }
      );
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

  rp2350BootloaderChecks =
    mkBootloaderChecks "rp2350-bootloader" deps.thumbv8mBootloaderArtifacts
      "thumbv8m.main-none-eabihf";
  rp2040BootloaderChecks =
    mkBootloaderChecks "rp2040-bootloader" deps.thumbv6mBootloaderArtifacts
      "thumbv6m-none-eabi";

  # esp32s3 firmware gates, grouped so they can be dropped as a unit when
  # the board is not a member. They share the esp toolchain and the
  # esp32s3 source scaffolding, so the esp backend of ubos-ota and the
  # esp32s3-tsens lib are gated here too: a tree without the board pulls
  # in no Xtensa toolchain at all.
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
      // webuiEnvAttrs
      // webuiDistEnv
    );

    # No codegen bag to reuse; see the esp bag rationale in nix/deps.nix.
    # The size guard must live here, not only in packages.esp32s3-image:
    # `nix flake check` builds checks.* but merely evaluates packages.*,
    # so a guard in the image package alone would never gate an oversized
    # image in CI.
    build-esp32s3 = espCraneLib.cargoBuild (
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
      // webuiEnvAttrs
      // webuiDistEnv
    );

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
      // webuiEnvAttrs
      // webuiDistEnv
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
// lib.optionalAttrs (hasCrate "rp2350-bootloader") rp2350BootloaderChecks
// lib.optionalAttrs (hasCrate "rp2040-bootloader") rp2040BootloaderChecks
// lib.optionalAttrs (hasCrate "esp32s3") espChecks
// rec {
  clippy-rp235x-sha256 = craneLib.cargoClippy {
    src = srcFor [ "rp235x-sha256" ];
    cargoArtifacts = deps.thumbv8mArtifacts;
    cargoExtraArgs = "-p rp235x-sha256 --target thumbv8m.main-none-eabihf";
    cargoClippyExtraArgs = "-- -D warnings";
    doInstallCargoArtifacts = false;
  };

  build-rp235x-sha256 = craneLib.cargoBuild {
    src = srcFor [ "rp235x-sha256" ];
    cargoArtifacts = deps.thumbv8mArtifacts;
    cargoBuildCommand = "cargo build --release";
    cargoExtraArgs = "-p rp235x-sha256 --target thumbv8m.main-none-eabihf";
    doInstallCargoArtifacts = false;
  };

  # ubos-ota is linted per backend here but has no standalone build
  # derivation: every firmware board depends on it, so each board build
  # compiles it transitively and a real breakage still fails the gate.
  # clippy-ubos-ota lints the rp backend; clippy-ubos-ota-esp below
  # lints the esp backend, since clippy only lints the top crate.
  clippy-ubos-ota = craneLib.cargoClippy {
    src = srcFor [ "ubos-ota" ];
    cargoArtifacts = deps.ubosOtaArtifacts;
    cargoExtraArgs = "-p ubos-ota --target thumbv8m.main-none-eabihf --features rp,embassy-rp/rp235xa,embassy-rp/imagedef-none";
    cargoClippyExtraArgs = "-- -D warnings";
    doInstallCargoArtifacts = false;
    CC_thumbv8m_main_none_eabihf = "${pkgs.gcc-arm-embedded}/bin/arm-none-eabi-gcc";
  };

  # ubos-rp is linted here and, like ubos-ota, has no standalone build
  # derivation: every RP board depends on it, so each board build compiles it
  # transitively. clippy selects a chip on the CLI because the crate pins none,
  # so the same source builds under both rp2040 and rp235xa.
  clippy-ubos-rp = craneLib.cargoClippy {
    src = srcFor [ "ubos-rp" ];
    cargoArtifacts = deps.ubosOtaArtifacts;
    cargoExtraArgs = "-p ubos-rp --target thumbv8m.main-none-eabihf --features embassy-rp/rp235xa,embassy-rp/imagedef-none";
    cargoClippyExtraArgs = "-- -D warnings";
    doInstallCargoArtifacts = false;
    CC_thumbv8m_main_none_eabihf = "${pkgs.gcc-arm-embedded}/bin/arm-none-eabi-gcc";
  };

  # ubos-stm32 is linted here for the same reason as ubos-rp: both STM32
  # boards depend on it, so each board build compiles it transitively.
  # clippy selects a chip on the CLI because the crate pins none, so the
  # same source builds under both stm32f407 and stm32f439.
  clippy-ubos-stm32 = craneLib.cargoClippy {
    src = srcFor [ "ubos-stm32" ];
    cargoArtifacts = deps.stm32f439Artifacts;
    cargoExtraArgs = "-p ubos-stm32 --target thumbv7em-none-eabihf --features embassy-stm32/stm32f439zi";
    cargoClippyExtraArgs = "-- -D warnings";
    doInstallCargoArtifacts = false;
    CC_thumbv7em_none_eabihf = "${pkgs.gcc-arm-embedded}/bin/arm-none-eabi-gcc";
  };

  build-ubos-firmware = craneLib.cargoBuild {
    src = srcFor [ "ubos-firmware" ];
    cargoArtifacts = deps.ubosFirmwareArtifacts;
    cargoBuildCommand = "cargo build --release";
    cargoExtraArgs = "-p ubos-firmware --target thumbv8m.main-none-eabihf";
    doInstallCargoArtifacts = false;
    nativeBuildInputs = [ pkgs.gcc-arm-embedded ];
    CC_thumbv8m_main_none_eabihf = "${pkgs.gcc-arm-embedded}/bin/arm-none-eabi-gcc";
  };

  clippy-ubos-firmware = craneLib.cargoClippy {
    src = srcFor [ "ubos-firmware" ];
    cargoArtifacts = deps.ubosFirmwareArtifacts;
    cargoExtraArgs = "-p ubos-firmware --target thumbv8m.main-none-eabihf";
    cargoClippyExtraArgs = "-- -D warnings";
    doInstallCargoArtifacts = false;
    nativeBuildInputs = [ pkgs.gcc-arm-embedded ];
    CC_thumbv8m_main_none_eabihf = "${pkgs.gcc-arm-embedded}/bin/arm-none-eabi-gcc";
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
    // webuiEnvAttrs
    // webuiDistEnv
  );

  fmt = craneLib.cargoFmt {
    inherit src;
    doInstallCargoArtifacts = false;
  };

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
    + lib.optionalString (hasCrate "braiins-axe") "  check ${
        root + "/firmware/braiins-axe/src/config.rs"
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
      # The block is 18 lines. The rest of the budget covers a shebang or
      # an attribute above it on a file that grows one.
      headLines = 25;
      grant = "GNU General Public License as published by";
      reservation = "reserves the right to grant any party a license";
      names = lib.concatMapStringsSep " -o " (e: "-name '*.${e}'") extensions;
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
      braiins-axe = [ "rgb_task" ];
    };
  };

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
  rp2350-bootloader-run = pkgs.callPackage ../pkgs/rp2350-bootloader-runner { };
  setup-network = pkgs.callPackage ../pkgs/setup-network { };
  host-runner-run = pkgs.callPackage ../pkgs/host-runner-run { };
}
