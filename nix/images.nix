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
  srcFor,
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
  # Workspace package names present in the tree; see nix/checks.nix. A
  # board whose crate is absent gates its image and UF2 outputs out. A
  # combined UF2 additionally needs its bootloader crate, so a board
  # present without its bootloader keeps its raw image but loses its UF2.
  members,
}:
let
  inherit (pkgs) lib;
  hasCrate = name: builtins.elem name members;
  inherit (helpers)
    armCc
    webuiEnvAttrs
    webuiDistEnv
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
        src = srcFor [ name ];
        cargoArtifacts = artifacts;
        cargoBuildCommand = "cargo build --release";
        cargoExtraArgs = "-p ${name} --target ${target}";
        nativeBuildInputs = [
          pkgs.gcc-arm-embedded
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
      // webuiDistEnv
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
        src = srcFor [ name ];
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

  braiinsAxeImage = mkArmImage {
    name = "braiins-axe";
    artifacts = deps.thumbv8mArtifacts;
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
      src = srcFor [ "esp32s3" ];
      cargoArtifacts = null;
      cargoVendorDir = espCargoVendorDir;
      cargoBuildCommand = "cargo build --release";
      cargoExtraArgs = "-p esp32s3@0.1.0 --target xtensa-esp32s3-none-elf -Z build-std=core";
      nativeBuildInputs = [
        xtensaEspToolchain
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
    // webuiDistEnv
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

  braiinsAxeUf2 = mkCombinedUf2 {
    name = "braiins-axe";
    appBin = "${braiinsAxeImage}/braiins-axe-v${fwVersion}.bin";
    bootElf = rp2350Bootloader;
    family = "rp2350-arm-s";
  };

  # A raw image is gated on its board being a member; a combined UF2
  # additionally needs its bootloader crate. In this repo every crate is
  # a member and every output is present.
  imageOutputs =
    drvOf:
    lib.optionalAttrs (hasCrate "rp2350") { rp2350-image = drvOf "rp2350-image"; }
    // lib.optionalAttrs (hasCrate "bmm-pico") { bmm-pico-image = drvOf "bmm-pico-image"; }
    // lib.optionalAttrs (hasCrate "rp2040") { rp2040-image = drvOf "rp2040-image"; }
    // lib.optionalAttrs (hasCrate "hashnode") { hashnode-image = drvOf "hashnode-image"; }
    // lib.optionalAttrs (hasCrate "braiins-axe") { braiins-axe-image = drvOf "braiins-axe-image"; }
    // lib.optionalAttrs (hasCrate "esp32s3") { esp32s3-image = drvOf "esp32s3-image"; }
    // lib.optionalAttrs (hasCrate "rp2350" && hasCrate "rp2350-bootloader") {
      rp2350-uf2 = drvOf "rp2350-uf2";
    }
    // lib.optionalAttrs (hasCrate "bmm-pico" && hasCrate "rp2350-bootloader") {
      bmm-pico-uf2 = drvOf "bmm-pico-uf2";
    }
    // lib.optionalAttrs (hasCrate "rp2040" && hasCrate "rp2040-bootloader") {
      rp2040-uf2 = drvOf "rp2040-uf2";
    }
    // lib.optionalAttrs (hasCrate "hashnode" && hasCrate "rp2350-bootloader") {
      hashnode-uf2 = drvOf "hashnode-uf2";
    }
    // lib.optionalAttrs (hasCrate "braiins-axe" && hasCrate "rp2350-bootloader") {
      braiins-axe-uf2 = drvOf "braiins-axe-uf2";
    };

  # Package and app metadata per output key: the built derivation, the
  # copy-app board name, extension, and label.
  meta = {
    rp2350-image = {
      drv = rp2350Image;
      name = "rp2350";
      ext = "bin";
      what = "image";
    };
    bmm-pico-image = {
      drv = bmmPicoImage;
      name = "bmm-pico";
      ext = "bin";
      what = "image";
    };
    rp2040-image = {
      drv = rp2040Image;
      name = "rp2040";
      ext = "bin";
      what = "image";
    };
    hashnode-image = {
      drv = hashnodeImage;
      name = "hashnode";
      ext = "bin";
      what = "image";
    };
    braiins-axe-image = {
      drv = braiinsAxeImage;
      name = "braiins-axe";
      ext = "bin";
      what = "image";
    };
    esp32s3-image = {
      drv = esp32s3Image;
      name = "esp32s3";
      ext = "bin";
      what = "image";
    };
    rp2350-uf2 = {
      drv = rp2350Uf2;
      name = "rp2350";
      ext = "uf2";
      what = "UF2";
    };
    bmm-pico-uf2 = {
      drv = bmmPicoUf2;
      name = "bmm-pico";
      ext = "uf2";
      what = "UF2";
    };
    rp2040-uf2 = {
      drv = rp2040Uf2;
      name = "rp2040";
      ext = "uf2";
      what = "UF2";
    };
    hashnode-uf2 = {
      drv = hashnodeUf2;
      name = "hashnode";
      ext = "uf2";
      what = "UF2";
    };
    braiins-axe-uf2 = {
      drv = braiinsAxeUf2;
      name = "braiins-axe";
      ext = "uf2";
      what = "UF2";
    };
  };
in
{
  packages = imageOutputs (key: meta.${key}.drv);

  apps = imageOutputs (
    key:
    mkCopyApp {
      inherit (meta.${key})
        name
        drv
        ext
        what
        ;
    }
  );
}
