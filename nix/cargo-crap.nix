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

# cargo-crap from its crates.io release, for the CRAP report over each
# host crate's coverage. It scores every function from its cyclomatic
# complexity and its line coverage, so a function that is both complex
# and untested stands out. nixpkgs does not carry it. The optional
# duplicate triage, which calls a remote service, stays off, so the
# build reaches no network.
{ pkgs }:
pkgs.rustPlatform.buildRustPackage rec {
  pname = "cargo-crap";
  version = "0.9.0";
  src = pkgs.fetchCrate {
    inherit pname version;
    hash = "sha256-agntJXi0OaYoYm/BOHWNGpGAHrvSUvk3BL+p0FpojDI=";
  };
  cargoHash = "sha256-cXwnuQDhEBLLS32G90q+b/pvnb0rcrTsMOCMQTMwlmk=";
}
