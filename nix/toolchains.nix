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
# compilation universe: the ARM boards, the ESP32-S3 shell's host rust,
# the redacted, the native host, and the BMM miniminer musl cross target.
# The Xtensa build toolchain itself lives in nix/esp.nix; the
# espRustToolchain here only equips the esp devshell with a host rustc
# and rust-analyzer while espup manages the Xtensa compiler.
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
  rustToolchain = pkgs.rust-bin.stable.latest.default.override {
    extensions = [
      "rust-src"
      "rust-analyzer"
      "rustfmt"
      "llvm-tools"
    ];
    targets = [
      "thumbv6m-none-eabi"
      "thumbv7em-none-eabihf"
      "thumbv8m.main-none-eabihf"
    ];
  };

  espRustToolchain = pkgs.rust-bin.stable.latest.default.override {
    extensions = [
      "rust-src"
      "rust-analyzer"
    ];
  };

  # redacted redacted, dual-core RV64GC. Targets the tier-2
  # `riscv64gc-unknown-none-elf` triple, which ships a prebuilt rust-std,
  # so it needs no `build-std`; `rust-src` is kept for rust-analyzer.
  riscvRustToolchain = pkgs.rust-bin.stable.latest.default.override {
    extensions = [
      "rust-src"
      "rust-analyzer"
      "rustfmt"
      "llvm-tools"
    ];
    targets = [ "riscv64gc-unknown-none-elf" ];
  };

  hostRustToolchain = pkgs.rust-bin.stable.latest.default.override {
    extensions = [
      "rust-src"
      "rust-analyzer"
      "rustfmt"
      "llvm-tools"
    ];
  };

  bmmRustToolchain = pkgs.rust-bin.stable.latest.default.override {
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

  # redacted redacted, RV64GC. The closure has no C deps, such as
  # secp256k1-sys/ii-sv2, so unlike espCraneLib it needs no cross GCC.
  riscvCraneLib = (crane.mkLib pkgs).overrideToolchain riscvRustToolchain;

  # Bare-metal riscv64 GCC, pulled into the private redacted-rtt / riscv board
  # check closure purely for its bundled `riscv64-none-elf-ld`; see
  # `riscvDefmtRustflags`. The redacted-rtt crate has no C deps, so this is the
  # linker, not a C toolchain.
  riscvGcc = pkgs.pkgsCross.riscv64-embedded.buildPackages.gcc;

  # redacted-rtt builds with defmt, whose `.defmt` symbol table lives in a
  # non-`SHF_ALLOC` section flagged `INFO` with no real load address.
  # rust-lld mishandles `R_RISCV_PCREL_HI20` relocations against it,
  # computing the auipc displacement as if it were a nearby real symbol
  # and clamping to a signed 20-bit range instead of the actual 32-bit
  # auipc+lo12 reach. A consuming firmware build carries the same flags in
  # its .cargo/config.toml. `riscvGcc` supplies the real
  # `riscv64-none-elf-ld` these resolve to.
  riscvDefmtRustflags = "-C panic=abort -C linker=riscv64-none-elf-ld -C linker-flavor=ld";
}
