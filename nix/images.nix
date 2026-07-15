# Production firmware images, bootloader ELFs, combined BOOTSEL UF2
# images, and the `nix run` apps that copy them into the repo's
# build/ directory.
#
# Build with a version number:
#   UBOS_FW_VERSION=42 nix build --impure .#rp2350-image
# This produces result/rp2350-v42.bin.
#
#   UBOS_FW_VERSION=42 nix build --impure .#rp2350-uf2
# This produces result/rp2350-v42.uf2.
#
# A pure build with no --impure defaults to version 1 because
# builtins.getEnv returns "" in the Nix sandbox; the sandbox
# strips all caller environment variables.
{
  pkgs,
  src,
  deps,
  craneLib,
  espCraneLib,
  espCargoVendorDir,
  xtensaEspToolchain,
  guards,
  helpers,
  fwVersion,
  slotMaxBytes,
  esp32s3SlotBytes,
}:
let
  inherit (helpers)
    armCc
    webuiEnvAttrs
    espCrossEnv
    espRustflags
    ;

  # Production ARM firmware image: build the board crate, objcopy the
  # raw binary into $out, and run the slot-size and pool-worker guards
  # on the artifact that ships. The guards mirror the build-<name>
  # checks; the difference is that the bin is kept in $out instead of
  # being deleted. The per-crate .cargo/config.toml, which bakes a
  # test POOL_USER for dev flashing on some boards, is not read here:
  # crane runs cargo from the flake root, so the default empty worker
  # is used and the guard proves it stayed out.
  mkArmImage =
    {
      name,
      artifacts,
      target,
    }:
    craneLib.cargoBuild (
      {
        inherit src;
        cargoArtifacts = artifacts;
        cargoBuildCommand = "cargo build --release";
        cargoExtraArgs = "-p ${name} --target ${target}";
        nativeBuildInputs = [
          pkgs.gcc-arm-embedded
          pkgs.esbuild
        ];
        UBOS_FW_VERSION = fwVersion;
        # Do not copy the cargo target directory into $out; we only
        # want the raw binary image.
        doInstallCargoArtifacts = false;
        installPhaseCommand = ''
          mkdir -p "$out"
          arm-none-eabi-objcopy -O binary \
            target/${target}/release/${name} \
            "$out/${name}-v${fwVersion}.bin"
        ''
        + guards.imageGuard {
          inherit name;
          bin = ''"$out/${name}-v${fwVersion}.bin"'';
          maxBytes = slotMaxBytes;
        };
      }
      // armCc target
      // webuiEnvAttrs
    );

  # Bootloader ELF builder. The bootloader occupies flash offset
  # 0x10000000 and carries the image-def the bootrom validates. The
  # combined UF2 derivations bundle this ELF with the app binary so
  # the image is drag-droppable via the BOOTSEL mass-storage
  # interface.
  mkBootloaderElf =
    {
      name,
      artifacts,
      target,
    }:
    craneLib.cargoBuild (
      {
        inherit src;
        cargoArtifacts = artifacts;
        cargoBuildCommand = "cargo build --release";
        cargoExtraArgs = "-p ${name} --target ${target}";
        nativeBuildInputs = [ pkgs.gcc-arm-embedded ];
        doInstallCargoArtifacts = false;
        installPhaseCommand = ''
          mkdir -p "$out"
          cp target/${target}/release/${name} "$out/boot.elf"
        '';
      }
      // armCc target
    );

  # Build a combined BOOTSEL UF2 image from a raw app binary and a
  # bootloader ELF. The bootloader covers offset 0x10000000, the
  # offset-0 region the bootrom validates; the app binary is placed at
  # 0x10021000, the ACTIVE partition. UF2 blocks carry individual
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

  # Copy the built image or UF2 into the repo's build/ directory and
  # print the absolute path of the written file as the last stdout
  # line, so the artifact survives `nix run` without dangling
  # /nix/store symlinks.
  mkCopyApp =
    {
      name,
      drv,
      ext,
      what,
    }:
    {
      type = "app";
      program = toString (
        pkgs.writeShellScript "${name}-${ext}-to-build" ''
          set -euo pipefail
          root="$(${pkgs.git}/bin/git rev-parse --show-toplevel)"
          dest="$root/build"
          mkdir -p "$dest"
          # The package $out holds exactly one ${name}-v<N>.${ext}.
          src_file="$(echo ${drv}/*.${ext})"
          name="$(basename "$src_file")"
          install -m644 "$src_file" "$dest/$name"
          echo "${name} ${what} v${fwVersion} written to:" >&2
          echo "$dest/$name"
        ''
      );
      meta.description = "Copy the ${name} ${what} into the repo build/ directory";
    };

  rp2350Image = mkArmImage {
    name = "rp2350";
    artifacts = deps.thumbv8mArtifacts;
    target = "thumbv8m.main-none-eabihf";
  };

  bmmPicoImage = mkArmImage {
    name = "bmm-pico";
    artifacts = deps.thumbv8mArtifacts;
    target = "thumbv8m.main-none-eabihf";
  };

  rp2040Image = mkArmImage {
    name = "rp2040";
    artifacts = deps.thumbv6mArtifacts;
    target = "thumbv6m-none-eabi";
  };

  hashnodeImage = mkArmImage {
    name = "hashnode";
    artifacts = deps.hashnodeArtifacts;
    target = "thumbv8m.main-none-eabihf";
  };

  # Production esp32s3 firmware image. espflash converts the linked
  # ELF into the esp-idf app-image format the OTA endpoint writes into
  # an ota_N slot. The size guard uses the 3 MiB app-slot budget,
  # esp32s3SlotBytes, which is what the REST handler enforces on the
  # upload for this board.
  esp32s3Image = espCraneLib.cargoBuild (
    espCrossEnv
    // espRustflags
    // {
      inherit src;
      cargoArtifacts = null;
      cargoVendorDir = espCargoVendorDir;
      cargoBuildCommand = "cargo build --release";
      cargoExtraArgs = "-p esp32s3@0.1.0 --target xtensa-esp32s3-none-elf -Z build-std=core";
      nativeBuildInputs = [
        xtensaEspToolchain
        pkgs.esbuild
        pkgs.espflash
      ];
      UBOS_FW_VERSION = fwVersion;
      doInstallCargoArtifacts = false;
      installPhaseCommand = ''
        mkdir -p "$out"
        # espflash reads/writes a user cache dir; keep it in the sandbox.
        export HOME="$TMPDIR"
        espflash save-image --chip esp32s3 --flash-size 8mb \
          target/xtensa-esp32s3-none-elf/release/esp32s3 \
          "$out/esp32s3-v${fwVersion}.bin"
      ''
      + guards.imageGuard {
        name = "esp32s3";
        bin = ''"$out/esp32s3-v${fwVersion}.bin"'';
        maxBytes = esp32s3SlotBytes;
        budgetLabel = "OTA app slot";
      };
    }
    // webuiEnvAttrs
  );

  # Bootloader for the rp2350, bmm-pico, and hashnode boards, all
  # built from the rp2350-bootloader crate. It is built with the
  # rp235xa feature but touches no GPIO, so the binary also runs on
  # the hashnode's RP2350B.
  rp2350Bootloader = mkBootloaderElf {
    name = "rp2350-bootloader";
    artifacts = deps.thumbv8mBootloaderArtifacts;
    target = "thumbv8m.main-none-eabihf";
  };

  rp2040Bootloader = mkBootloaderElf {
    name = "rp2040-bootloader";
    artifacts = deps.thumbv6mBootloaderArtifacts;
    target = "thumbv6m-none-eabi";
  };

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
in
{
  packages = {
    rp2350-image = rp2350Image;
    bmm-pico-image = bmmPicoImage;
    rp2040-image = rp2040Image;
    hashnode-image = hashnodeImage;
    rp2350-uf2 = rp2350Uf2;
    bmm-pico-uf2 = bmmPicoUf2;
    rp2040-uf2 = rp2040Uf2;
    hashnode-uf2 = hashnodeUf2;
    esp32s3-image = esp32s3Image;
  };

  apps = {
    rp2350-image = mkCopyApp {
      name = "rp2350";
      drv = rp2350Image;
      ext = "bin";
      what = "image";
    };
    bmm-pico-image = mkCopyApp {
      name = "bmm-pico";
      drv = bmmPicoImage;
      ext = "bin";
      what = "image";
    };
    rp2040-image = mkCopyApp {
      name = "rp2040";
      drv = rp2040Image;
      ext = "bin";
      what = "image";
    };
    hashnode-image = mkCopyApp {
      name = "hashnode";
      drv = hashnodeImage;
      ext = "bin";
      what = "image";
    };
    rp2350-uf2 = mkCopyApp {
      name = "rp2350";
      drv = rp2350Uf2;
      ext = "uf2";
      what = "UF2";
    };
    bmm-pico-uf2 = mkCopyApp {
      name = "bmm-pico";
      drv = bmmPicoUf2;
      ext = "uf2";
      what = "UF2";
    };
    rp2040-uf2 = mkCopyApp {
      name = "rp2040";
      drv = rp2040Uf2;
      ext = "uf2";
      what = "UF2";
    };
    hashnode-uf2 = mkCopyApp {
      name = "hashnode";
      drv = hashnodeUf2;
      ext = "uf2";
      what = "UF2";
    };
    esp32s3-image = mkCopyApp {
      name = "esp32s3";
      drv = esp32s3Image;
      ext = "bin";
      what = "image";
    };
  };
}
