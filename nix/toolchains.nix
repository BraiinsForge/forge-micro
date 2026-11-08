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

# Rust toolchains and the crane instances built on them, one per
# compilation universe: the ARM boards, the native host, and the BMM
# miniminer musl cross target. The Xtensa toolchain lives in nix/esp.nix.
{
  pkgs,
  nixpkgs,
  system,
  crane,
}:
let
  # Cross gcc for the BMM miniminer target. Rust emits the code,
  # gcc only links, so a generic ARM-musleabi toolchain works for
  # the Cortex-A9 device. The crossSystem is pinned to armv5tel
  # because that is the baseline soft-float ARMv5 musl variant that
  # matches Rust's `arm-unknown-linux-musleabi` ABI; nixpkgs rejects
  # the unversioned `arm-linux` system because musl's meta.platforms
  # lists only versioned variants. ARMv5 binaries run unchanged on
  # the Cortex-A9, ARMv7l. Off-Nix workflows that use the OpenWrt
  # SDK directly are unaffected; that path is user-local.
  pkgsBmm = import nixpkgs {
    inherit system;
    crossSystem = {
      config = "armv5tel-unknown-linux-musleabi";
    };
  };
in
rec {
  # The one Rust version every universe here compiles with. A consumer
  # that extends this set reads it for its own toolchains too, so the
  # version is exported rather than spelled at each use. `stable.latest`
  # stood here before, which let any rust-overlay bump move the compiler
  # under the firmware without a line of this repo changing. A version
  # written down moves when someone decides to move it.
  rustVersion = "1.98.1";

  rustToolchain = pkgs.rust-bin.stable.${rustVersion}.default.override {
    extensions = [
      "rust-src"
      "rust-analyzer"
      "rustfmt"
      "llvm-tools"
    ];
    targets = [
      "thumbv6m-none-eabi"
      "thumbv8m.main-none-eabihf"
    ];
  };

  hostRustToolchain = pkgs.rust-bin.stable.${rustVersion}.default.override {
    extensions = [
      "rust-src"
      "rust-analyzer"
      "rustfmt"
      "llvm-tools"
    ];
  };

  bmmRustToolchain = pkgs.rust-bin.stable.${rustVersion}.default.override {
    extensions = [
      "rust-src"
      "rust-analyzer"
      "rustfmt"
      "llvm-tools"
    ];
    targets = [ "arm-unknown-linux-musleabi" ];
  };

  bmmCc = pkgsBmm.stdenv.cc;

  craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;

  bmmCraneLib = (crane.mkLib pkgs).overrideToolchain bmmRustToolchain;

}
