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

# Upstream crates consumed through the workspace `[patch]` table as
# pinned pristine sources with local patch files applied, instead of
# vendored trees. The repo carries only the pin and the patches; nix
# materializes the patched tree at `patches/<crate>` for cargo — the
# dev shells symlink it into the working tree and nix/source.nix
# copies it into the cleaned check sources. The patch files live in
# `patches/` and keep the upstream license of the crate they patch.
{ pkgs }:
let
  # The edge-net workspace at the rev the workspace deps pin; the
  # edge-nal-embassy override below is cut from its subdir so the
  # whole edge-net family keeps resolving to one source.
  edge-net = pkgs.fetchFromGitHub {
    owner = "ivmarkov";
    repo = "edge-net";
    rev = "6f7e384ff738aef38f3c38a38225a6ed1cfbac20";
    hash = "sha256-38mEJoN6XMAO3yVM7EQ6oxPEJd0p+oOXz8trWfTtnPQ=";
  };
in
{
  # embassy-net-wiznet 0.3.0 with the read_frame hardening and the
  # RECV-interrupt drain rework applied in order; see the patch
  # preambles and the `[patch.crates-io]` comment in Cargo.toml for
  # why the base stays the crates.io release rather than a git rev of
  # the embassy workspace.
  embassy-net-wiznet = pkgs.applyPatches {
    name = "embassy-net-wiznet-0.3.0-patched";
    src = pkgs.fetchzip {
      name = "embassy-net-wiznet-0.3.0";
      url = "https://static.crates.io/crates/embassy-net-wiznet/embassy-net-wiznet-0.3.0.crate";
      extension = "tar.gz";
      hash = "sha256-EinaCoxc1IIotH9RHj9iBzpY0VDs/zm+C+nqHNp94E8=";
    };
    patches = [
      ../patches/embassy-net-wiznet-rx-frame-underflow.patch
      ../patches/embassy-net-wiznet-drain-rx-frames.patch
    ];
  };

  # esp-hal 1.2.2 with the pre-init literals kept in the RAM text
  # section, without which the ESP32-S3 image does not link; see the
  # patch preamble.
  esp-hal = pkgs.applyPatches {
    name = "esp-hal-1.2.2-patched";
    src = pkgs.fetchzip {
      name = "esp-hal-1.2.2";
      url = "https://static.crates.io/crates/esp-hal/esp-hal-1.2.2.crate";
      extension = "tar.gz";
      hash = "sha256-8eMojFBqNKrvvdcCmMAZYsV+MHPImb0b9zBj3pGHuqg=";
    };
    patches = [
      ../patches/esp-hal-pre-init-literals.patch
    ];
  };

  # smoltcp 0.13.1 with its generated configuration written in a fixed
  # order, so that two builds of it are the same crate; see the patch
  # preamble.
  smoltcp = pkgs.applyPatches {
    name = "smoltcp-0.13.1-patched";
    src = pkgs.fetchzip {
      name = "smoltcp-0.13.1";
      url = "https://static.crates.io/crates/smoltcp/smoltcp-0.13.1.crate";
      extension = "tar.gz";
      hash = "sha256-WJ1jKfaxikG7LqWqfOrCvrd+Fwiv7Tk6HubUXCnDAmo=";
    };
    patches = [
      ../patches/smoltcp-sorted-config.patch
    ];
  };

  # riscv-macros 0.3.0 with its interrupt code generated in a fixed
  # order, so that two builds of a crate using it are the same crate;
  # see the patch preamble.
  riscv-macros = pkgs.applyPatches {
    name = "riscv-macros-0.3.0-patched";
    src = pkgs.fetchzip {
      name = "riscv-macros-0.3.0";
      url = "https://static.crates.io/crates/riscv-macros/riscv-macros-0.3.0.crate";
      extension = "tar.gz";
      hash = "sha256-xnv7ptM3qxKQuFblommnT98SmmVMXJ9wGap5JOTEHeQ=";
    };
    patches = [
      ../patches/riscv-macros-sorted-interrupts.patch
    ];
  };

  # edge-nal-embassy from the pinned edge-net rev with the local patch
  # series applied in order; each patch's preamble explains its change.
  # The series starts with the standalone-manifest rewrite because the
  # subdir only resolves workspace-inherited deps inside the edge-net
  # workspace.
  edge-nal-embassy = pkgs.applyPatches {
    name = "edge-nal-embassy-0.9.0-patched";
    src = "${edge-net}/edge-nal-embassy";
    patches = [
      ../patches/edge-nal-embassy-01-standalone-manifest.patch
      ../patches/edge-nal-embassy-02-accept-peer-endpoint.patch
      ../patches/edge-nal-embassy-03-accept-reset-drop.patch
      ../patches/edge-nal-embassy-04-embassy-time-dep.patch
      ../patches/edge-nal-embassy-05-idle-timeout.patch
      ../patches/edge-nal-embassy-06-edition-2024.patch
      ../patches/edge-nal-embassy-07-accept-timeout.patch
    ];
  };
}
