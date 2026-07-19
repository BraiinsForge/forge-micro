# Shared dependency artifacts for the clippy, test, and build checks.
# Flake checks are per-crate; without reused artifacts each check
# recompiles the full dependency graph and dominates `nix flake check`
# wall time.
#
# The bags split host crates from firmware crates because cargo
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
{
  pkgs,
  src,
  loadTestSrc,
  craneLib,
  helpers,
  bmmCraneLib,
  espCraneLib,
  espCargoVendorDir,
  xtensaEspToolchain,
  bmmCc,
}:
let
  # A check-only bag: dependencies are `cargo check`ed, never linked.
  checkBag =
    lib: extra:
    lib.buildDepsOnly (
      {
        inherit src;
        doCheck = false;
        cargoBuildCommand = "true";
      }
      // extra
    );

  # A codegen bag for one ARM firmware target: dependencies are built
  # for real so the board release builds reuse the object code. The
  # cross gcc covers C build scripts, such as secp256k1-sys via ii-sv2.
  armBag =
    { crates, target }:
    craneLib.buildDepsOnly (
      {
        inherit src;
        doCheck = false;
        cargoBuildCommand = "cargo build --release";
        cargoExtraArgs = builtins.concatStringsSep " " (
          (map (c: "-p ${c}") crates) ++ [ "--target ${target}" ]
        );
        nativeBuildInputs = [ pkgs.gcc-arm-embedded ];
      }
      // helpers.armCc target
    );

  dev =
    bag:
    bag.overrideAttrs (_: {
      CARGO_PROFILE = "";
    });
in
rec {
  loadTestArtifacts = craneLib.buildDepsOnly {
    src = loadTestSrc;
    strictDeps = true;
  };
  loadTestArtifactsDev = dev loadTestArtifacts;

  hostArtifacts = checkBag craneLib {
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
  hostArtifactsDev = dev hostArtifacts;

  hostRunnerArtifacts = checkBag craneLib {
    cargoExtraArgs = "-p host-runner";
    # host-runner depends on `ubos-webui`, whose `build.rs` shells
    # out to `esbuild` to minify the dashboard and login assets.
    # `buildDepsOnly` still executes build scripts, so esbuild
    # must be on PATH here too.
    nativeBuildInputs = [ pkgs.esbuild ];
  };
  hostRunnerArtifactsDev = dev hostRunnerArtifacts;

  ubosThermalArtifacts = checkBag craneLib { cargoExtraArgs = "-p ubos-thermal"; };
  ubosThermalArtifactsDev = dev ubosThermalArtifacts;

  ubosConfigArtifacts = checkBag craneLib { cargoExtraArgs = "-p ubos-config"; };
  ubosConfigArtifactsDev = dev ubosConfigArtifacts;

  ubosConfigStdArtifacts = checkBag craneLib { cargoExtraArgs = "-p ubos-config --features std"; };
  ubosConfigStdArtifactsDev = dev ubosConfigStdArtifacts;

  ubosEventsArtifacts = checkBag craneLib { cargoExtraArgs = "-p ubos-events"; };
  ubosEventsStdArtifacts = checkBag craneLib { cargoExtraArgs = "-p ubos-events --features std"; };
  ubosEventsStdArtifactsDev = dev ubosEventsStdArtifacts;

  ubosCoolingArtifacts = checkBag craneLib { cargoExtraArgs = "-p ubos-cooling"; };
  ubosCoolingArtifactsDev = dev ubosCoolingArtifacts;

  ubosManagerArtifacts = checkBag craneLib { cargoExtraArgs = "-p ubos-manager"; };
  ubosManagerArtifactsDev = dev ubosManagerArtifacts;

  ubosHttpArtifacts = checkBag craneLib { cargoExtraArgs = "-p ubos-http"; };
  ubosHttpArtifactsDev = dev ubosHttpArtifacts;

  ubosApiArtifacts = checkBag craneLib { cargoExtraArgs = "-p ubos-api"; };
  ubosApiArtifactsDev = dev ubosApiArtifacts;

  thumbv6mArtifacts = armBag {
    crates = [ "rp2040" ];
    target = "thumbv6m-none-eabi";
  };

  # Separate dep bag for the rp2040 bootloader: it uses
  # `cortex-m/critical-section-single-core` rather than
  # `embassy-rp/critical-section-impl`, and cargo's per-invocation
  # feature unification would produce duplicate `RawRestoreStateInner`
  # definitions if both crate trees were mixed into one `buildDepsOnly`.
  thumbv6mBootloaderArtifacts = armBag {
    crates = [ "rp2040-bootloader" ];
    target = "thumbv6m-none-eabi";
  };

  # stm32f407 and stm32f439 cannot share a deps-only bag: each selects a
  # different chip feature on `stm32-metapac`, and cargo's per-invocation
  # feature unification activates both, which the metapac build script
  # rejects with "Multiple stm32xx Cargo features enabled".
  stm32f407Artifacts = armBag {
    crates = [ "stm32f407" ];
    target = "thumbv7em-none-eabihf";
  };

  stm32f439Artifacts = armBag {
    crates = [ "stm32f439" ];
    target = "thumbv7em-none-eabihf";
  };

  thumbv8mArtifacts = armBag {
    crates = [
      "bmm-pico"
      "rp2350"
      "rp235x-sha256"
    ];
    target = "thumbv8m.main-none-eabihf";
  };

  # hashnode cannot share the thumbv8m bag: it selects
  # embassy-rp/rp235xb while the bag's crates select rp235xa, so
  # the bag's embassy-rp would never match and every consumer of
  # a unified bag would rebuild it.
  hashnodeArtifacts = armBag {
    crates = [ "hashnode" ];
    target = "thumbv8m.main-none-eabihf";
  };

  ubosOtaArtifacts = checkBag craneLib (
    {
      cargoExtraArgs = "-p ubos-ota --target thumbv8m.main-none-eabihf --features rp,embassy-rp/rp235xa,embassy-rp/imagedef-none";
    }
    // helpers.armCc "thumbv8m.main-none-eabihf"
  );

  # ubos-firmware references the subsystem crates, which unify
  # firmware-only embassy features; it is gated on one ARM target
  # rather than the host bag for the same reason the firmware crates
  # are. No host test: the crate is all `const`.
  ubosFirmwareArtifacts = armBag {
    crates = [ "ubos-firmware" ];
    target = "thumbv8m.main-none-eabihf";
  };

  # Separate dep bag for the bootloader: it uses
  # `cortex-m/critical-section-single-core` rather than
  # `embassy-rp/critical-section-impl`, and cargo's per-invocation
  # feature unification would produce duplicate `RawRestoreStateInner`
  # definitions if both crate trees are mixed in one `buildDepsOnly`.
  thumbv8mBootloaderArtifacts = armBag {
    crates = [ "rp2350-bootloader" ];
    target = "thumbv8m.main-none-eabihf";
  };

  bmmArtifacts = checkBag bmmCraneLib {
    cargoExtraArgs = "-p host-runner --target arm-unknown-linux-musleabi";
    # `pkgs.esbuild` is needed for ubos-webui's build.rs, which
    # runs as part of host-runner's dependency closure.
    nativeBuildInputs = [
      bmmCc
      pkgs.esbuild
    ];
    CARGO_TARGET_ARM_UNKNOWN_LINUX_MUSLEABI_LINKER = "${bmmCc}/bin/armv5tel-unknown-linux-musleabi-gcc";
    ${helpers.ccVar "arm-unknown-linux-musleabi"} = "${bmmCc}/bin/armv5tel-unknown-linux-musleabi-gcc";
  };

  # The esp deps bag stays check-only. crane's buildDepsOnly stubs the
  # esp32s3 bin with a dummy `main`, and a real `cargo build` fails to
  # link that stub under `-Z build-std=core`: the esp crt0 references
  # `main` while the crate is `#![no_main]`. A `cargo check` never
  # links, so it builds. The esp release builds therefore keep
  # `cargoArtifacts = null` and rebuild their dependency closure; the
  # other targets' bags do codegen and the builds reuse them.
  espArtifacts = checkBag espCraneLib (
    {
      cargoVendorDir = espCargoVendorDir;
      cargoExtraArgs = builtins.concatStringsSep " " [
        "-p esp32s3@0.1.0"
        "-p esp32s3-tsens"
        "--target xtensa-esp32s3-none-elf"
        "-Z build-std=core"
      ];
      nativeBuildInputs = [ xtensaEspToolchain ];
    }
    // helpers.espCrossEnv
  );
}
