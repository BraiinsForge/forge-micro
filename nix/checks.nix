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
# `patches/edge-nal-embassy` and `patches/embassy-net-wiznet` are
# intentionally not gated by their own clippy or build derivations.
# They are vendored upstream crates carrying local fixes that we do not
# author; gating them in CI would tie the build to upstream lint
# cleanliness we do not control. Every firmware build derivation below
# exercises them transitively, so a real breakage still fails the gate.
{
  pkgs,
  src,
  srcFor,
  loadTestSrc,
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
}:
let
  inherit (pkgs) lib;
  inherit (helpers)
    armCc
    webuiEnvAttrs
    espCrossEnv
    espRustflags
    ;
  root = ../.;

  # Host-side clippy: one row per check. `crate` is the cargo package;
  # `artifacts` defaults to the shared host bag; `allTargets` is off for
  # crates whose tests or examples do not build on the host feature set;
  # `args` appends feature flags; `esbuild`/`webuiEnv` cover ubos-webui's
  # build.rs.
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
      // lib.optionalAttrs (spec.esbuild or false) { nativeBuildInputs = [ pkgs.esbuild ]; }
      // lib.optionalAttrs (spec.webuiEnv or false) webuiEnvAttrs
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
      // lib.optionalAttrs (spec.esbuild or false) { nativeBuildInputs = [ pkgs.esbuild ]; }
      // lib.optionalAttrs (spec.webuiEnv or false) webuiEnvAttrs
    );

  hostClippyChecks = builtins.mapAttrs (_: mkHostClippy) {
    clippy-ubos-core.crate = "ubos-core";
    clippy-ubos-http.crate = "ubos-http";
    clippy-ubos-api.crate = "ubos-api";
    clippy-ubos-auth.crate = "ubos-auth";
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
      # build.rs runs `esbuild --minify` on the dashboard CSS/JS
      # before gzipping into OUT_DIR.
      esbuild = true;
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
      # `pkgs.esbuild` is needed for ubos-webui's build.rs, which
      # runs as part of host-runner's dependency closure.
      esbuild = true;
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
      # build.rs runs `esbuild --minify` on the dashboard CSS/JS
      # before gzipping into OUT_DIR.
      esbuild = true;
      webuiEnv = true;
    };
    test-host-runner = {
      crate = "host-runner";
      artifacts = deps.hostRunnerArtifactsDev;
      # `pkgs.esbuild` is needed for ubos-webui's build.rs, which
      # runs as part of host-runner's dependency closure.
      esbuild = true;
      webuiEnv = true;
    };
    test-ubos-thermal = {
      crate = "ubos-thermal";
      artifacts = deps.ubosThermalArtifactsDev;
    };
    test-ubos-cooling = {
      crate = "ubos-cooling";
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
  boards = {
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
  # board check points it at arm-none-eabi-gcc. esbuild covers
  # ubos-webui's build.rs in each board's dependency closure.
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
          pkgs.esbuild
        ];
      }
      // armCc board.target
      // webuiEnvAttrs
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
          pkgs.esbuild
        ];
      }
      // armCc board.target
      // webuiEnvAttrs
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
  # esbuild or webui env, since ubos-webui is not in the closure, and
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
      pkgs.esbuild
    ];
  };

  rp2350BootloaderChecks =
    mkBootloaderChecks "rp2350-bootloader" deps.thumbv8mBootloaderArtifacts
      "thumbv8m.main-none-eabihf";
  rp2040BootloaderChecks =
    mkBootloaderChecks "rp2040-bootloader" deps.thumbv6mBootloaderArtifacts
      "thumbv6m-none-eabi";

in
hostClippyChecks
// hostTestChecks
// boardChecks
// rp2350BootloaderChecks
// rp2040BootloaderChecks
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
    }
    // webuiEnvAttrs
  );

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

  clippy-load-test = craneLib.cargoClippy {
    src = loadTestSrc;
    cargoArtifacts = deps.loadTestArtifacts;
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
    cargoArtifacts = deps.loadTestArtifactsDev;
    CARGO_PROFILE = "";
    strictDeps = true;
    doInstallCargoArtifacts = false;
  };

  # Aggregate over every host-testable crate check plus the load-test
  # crate, so one build forces them all; derived from the test table,
  # a new test row joins automatically. The web/host-runner tests sit
  # in web-host instead: they additionally need the flake-provided
  # esbuild and rebuild whenever the webui build env changes.
  fast-host =
    let
      fastTests = builtins.removeAttrs hostTestChecks [
        "test-ubos-webui"
        "test-host-runner"
      ];
    in
    pkgs.runCommand "ubos-main-fast-host-tests"
      {
        nativeBuildInputs = builtins.attrValues fastTests ++ [ test-load-test ];
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
          "These checks require the flake-provided esbuild dependency." \
          > "$out/README"
      '';

  # Aggregate over every firmware release build; derived from the board
  # table plus the hand-written build checks above.
  firmware-builds =
    pkgs.runCommand "ubos-main-firmware-builds"
      {
        nativeBuildInputs =
          builtins.attrValues (lib.filterAttrs (name: _: lib.hasPrefix "build-" name) boardChecks)
          ++ [
            build-esp32s3
            rp2350BootloaderChecks.build-rp2350-bootloader
            rp2040BootloaderChecks.build-rp2040-bootloader
            build-esp32s3-bootloader
          ];
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
    # fallback, and the RP boards all pin slotMaxKiB * 1024.
    check ${root + "/ubos/ubos-api/src/system.rs"}     '${toString slotMaxKiB} * 1024'
    check ${root + "/ubos/ubos-webui/src/index.js"}    '${toString slotMaxKiB} * 1024'
    check ${root + "/firmware/rp2040/src/config.rs"}   '${toString slotMaxKiB} * 1024'
    check ${root + "/firmware/rp2350/src/config.rs"}   '${toString slotMaxKiB} * 1024'
    check ${root + "/firmware/bmm-pico/src/config.rs"} '${toString slotMaxKiB} * 1024'
    check ${root + "/firmware/hashnode/src/config.rs"} '${toString slotMaxKiB} * 1024'
    # esp32s3: the 3 MiB app slot, not the shared RP budget.
    check ${root + "/firmware/esp32s3/src/config.rs"}  '3 * 1024 * 1024'
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

  # The patched probe-rs-tools-server is consumed only by the
  # NUC's nixosConfiguration, which nix flake check never builds,
  # so a patch that no longer applies or a probe-rs build break
  # would otherwise keep CI green. Realize the derivation here so
  # the check stage is its gate.
  probe-rs-tools-server = pkgs.probe-rs-tools-server;
}
