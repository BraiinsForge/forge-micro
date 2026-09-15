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

# ESP32-S3 build universe: the pinned esp-rs Xtensa rust toolchain,
# the crosstool-NG xtensa GCC for C deps, the crane instance and
# vendored dependency set built on them, the rollback-enabled ESP-IDF
# second-stage bootloader, and the espup binary patcher for the esp
# devshell.
{
  pkgs,
  crane,
  system,
  nixpkgs-esp-dev,
  # The one rust version nix/toolchains.nix pins. The esp-rs release
  # below is a build of it.
  rustVersion,
}:
rec {
  # Xtensa-LX7 Rust toolchain for the ESP32-S3. Upstream rustc does
  # not ship xtensa-esp32s3-none-elf; esp-rs publishes pre-built
  # artifacts at github.com/esp-rs/rust-build. Running espup inside
  # a Nix build is non-deterministic, so flake checks fetch the
  # release tarballs directly with pinned hashes.
  #
  # esp-rs names a release after the rust version it builds, with a
  # build counter appended, so the version here is the pinned one plus
  # that counter and not a second copy of it. A rust bump in
  # nix/toolchains.nix moves this toolchain with it, and needs the
  # counter of the matching esp-rs release and fresh hashes:
  #   nix-prefetch-url <rust tarball url>
  #   nix-prefetch-url <rust-src tarball url>
  #   nix hash convert --to sri --hash-algo sha256 <each base32>
  # The --unpack flag is intentionally omitted; fetchurl verifies
  # the compressed tarball directly.
  # Update the hashes below and retest both flake checks and the
  # esp devshell in the same commit.
  espBuild = "0";
  espToolchainVersion = "${rustVersion}.${espBuild}";

  espToolchain = pkgs.stdenv.mkDerivation {
    pname = "esp-rust-xtensa";
    version = espToolchainVersion;

    srcs = [
      (pkgs.fetchurl {
        url = "https://github.com/esp-rs/rust-build/releases/download/v${espToolchainVersion}/rust-${espToolchainVersion}-x86_64-unknown-linux-gnu.tar.xz";
        hash = "sha256-u38+zDa3QdyfVlVGKST+k6hiDXw0nFn5moZIkoztmpo=";
      })
      (pkgs.fetchurl {
        url = "https://github.com/esp-rs/rust-build/releases/download/v${espToolchainVersion}/rust-src-${espToolchainVersion}.tar.xz";
        hash = "sha256-wZ5aQKYl32SEVKlyovsvMDFE1CcEraeHzco6D1nKDMw=";
      })
    ];

    nativeBuildInputs = [ pkgs.autoPatchelfHook ];
    buildInputs = [
      pkgs.stdenv.cc.cc
      pkgs.zlib
    ];

    sourceRoot = ".";
    dontStrip = true;

    # Both tarballs unpack to directories named rust-nightly-*
    # regardless of the release version, because esp-rs builds
    # from the upstream rust master branch.
    installPhase = ''
      runHook preInstall
      for dir in rust-nightly-x86_64-unknown-linux-gnu rust-src-nightly; do
        patchShebangs "$dir/install.sh"
        "$dir/install.sh" \
          --destdir=$out --prefix="" --disable-ldconfig \
          --without=rust-docs-json-preview,rust-docs
      done
      runHook postInstall
    '';
  };

  espCraneLib = (crane.mkLib pkgs).overrideToolchain espToolchain;

  # Xtensa GCC toolchain for `secp256k1-sys` and any other C
  # dependency in the esp32s3 build. `secp256k1-sys` arrives via
  # the unconditional `ii-sv2` dep. Nixpkgs doesn't ship
  # `xtensa-esp-elf-gcc`, and the rustup-managed copy from `espup`
  # is invisible inside the flake check sandbox. Hashes paired with
  # the URL; bump both when upgrading to a newer
  # espressif/crosstool-NG release.
  xtensaEspToolchain = pkgs.stdenv.mkDerivation {
    pname = "xtensa-esp-elf";
    version = "15.2.0_20250920";

    src = pkgs.fetchurl {
      url = "https://github.com/espressif/crosstool-NG/releases/download/esp-15.2.0_20250920/xtensa-esp-elf-15.2.0_20250920-x86_64-linux-gnu.tar.xz";
      hash = "sha256-49d60UVEgUUnu+ei0PeexFkqTiM5LFHHOIwOaGtqaXc=";
    };

    nativeBuildInputs = [ pkgs.autoPatchelfHook ];
    buildInputs = [
      pkgs.stdenv.cc.cc
      pkgs.zlib
    ];

    sourceRoot = "xtensa-esp-elf";
    dontStrip = true;

    installPhase = ''
      runHook preInstall
      mkdir -p "$out"
      cp -r . "$out"
      runHook postInstall
    '';
  };

  # build-std=core pulls sysroot crates like proc_macro and std
  # that have their own Cargo.lock outside our tree. Vendor both
  # so the Xtensa clippy checks resolve without network access.
  espCargoVendorDir = espCraneLib.vendorMultipleCargoDeps {
    cargoLockList = [
      ../Cargo.lock
      "${espToolchain}/lib/rustlib/src/rust/library/Cargo.lock"
    ];
  };

  # Second-stage bootloader for the esp32s3 board, compiled from the
  # minimal ESP-IDF stub in firmware/esp32s3-bootloader. ESP-IDF ships
  # no Rust bootloader for Xtensa, and the bootloader bundled by
  # espflash/probe-rs has CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE off, so
  # the OTA PendingVerify auto-revert never fires. We build our own with
  # rollback on and flash it via `probe-rs run --idf-bootloader`.
  esp32s3Bootloader = pkgs.stdenv.mkDerivation {
    pname = "esp32s3-bootloader";
    version = "5.5.2";
    src = ../firmware/esp32s3-bootloader;
    # esp-idf-xtensa carries a setup-hook that exports IDF_PATH and
    # puts idf.py plus the xtensa toolchain on PATH. It covers the whole
    # Xtensa family: esp32/s2/s3; the per-chip esp-idf-esp32s3 it
    # replaces is deprecated as of ESP-IDF 6.0.
    nativeBuildInputs = [ nixpkgs-esp-dev.packages.${system}.esp-idf-xtensa ];
    dontConfigure = true;
    buildPhase = ''
      runHook preBuild
      export HOME="$TMPDIR"
      cp -r "$src" project
      chmod -R u+w project
      cd project
      idf.py set-target esp32s3
      idf.py bootloader
      runHook postBuild
    '';
    installPhase = ''
      runHook preInstall
      mkdir -p "$out"
      cp build/bootloader/bootloader.bin "$out/"
      runHook postInstall
    '';
  };

  patchEspBinaries = pkgs.writeShellScriptBin "patch-esp-binaries" ''
    if [ -f /etc/NIXOS ] || [ ! -f /lib64/ld-linux-x86-64.so.2 ]; then
      echo "Patching ESP toolchain binaries for NixOS..."
      find "$RUSTUP_HOME/toolchains/esp" -type f | while read -r file; do
        if file "$file" 2>/dev/null | grep -q 'ELF'; then
          ${pkgs.patchelf}/bin/patchelf \
            --set-interpreter "$(cat ${pkgs.stdenv.cc}/nix-support/dynamic-linker)" \
            "$file" 2>/dev/null || true
          ${pkgs.patchelf}/bin/patchelf \
            --set-rpath "${
              pkgs.lib.makeLibraryPath [
                pkgs.stdenv.cc.cc.lib
                pkgs.glibc
                pkgs.zlib
                pkgs.libxml2
                pkgs.openssl
                pkgs.libffi
              ]
            }:$RUSTUP_HOME/toolchains/esp/lib:$(dirname "$file")" \
            "$file" 2>/dev/null || true
        fi
      done
      echo "Patching complete."
    fi
  '';
}
