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
# `patches/` and keep the upstream license, like the probe-rs ones.
{ pkgs }:
{
  # embassy-net-wiznet 0.3.0 with the read_frame hardening; see the
  # patch preamble and the `[patch.crates-io]` comment in Cargo.toml
  # for why the base stays the crates.io release rather than a git
  # rev of the embassy workspace.
  embassy-net-wiznet = pkgs.applyPatches {
    name = "embassy-net-wiznet-0.3.0-patched";
    src = pkgs.fetchzip {
      name = "embassy-net-wiznet-0.3.0";
      url = "https://static.crates.io/crates/embassy-net-wiznet/embassy-net-wiznet-0.3.0.crate";
      extension = "tar.gz";
      hash = "sha256-EinaCoxc1IIotH9RHj9iBzpY0VDs/zm+C+nqHNp94E8=";
    };
    patches = [ ../patches/embassy-net-wiznet-rx-frame-underflow.patch ];
  };
}
