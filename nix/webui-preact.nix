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

# Hermetic Rsbuild/Preact build for `ubos/ubos-webui-preact`.
#
# This produces the two single-file pages the crate's build.rs embeds:
# `$out/index.html` and `$out/login.html`. flake.nix builds this in the
# per-system `let` block and threads it into nix/lib.nix as
# `preactDistEnv`, which the ubos-webui-preact checks set as
# `UBOS_WEBUI_PREACT_DIST` so the Rust check sandbox never needs Node —
# build.rs just gzips these two files.
#
# `npmDepsHash` is a hash over package-lock.json's dependency set. It
# needs a network fetch that only runs under Nix, so regenerate it
# whenever the lockfile moves:
#   nix run nixpkgs#prefetch-npm-deps -- \
#     ubos/ubos-webui-preact/package-lock.json
# Paste the printed hash into `npmDepsHash` below. A stale hash fails
# the build and prints the correct "got:" hash to copy.
#
# The dev shells already carry `nodejs` (nix/shells.nix), so
# `npm run dev` and build.rs's dev-shell fallback work without this
# derivation; this module is only for the gated, offline flake build.

{
  lib,
  buildNpmPackage,
  webuiBuildEnv,
}:

buildNpmPackage {
  pname = "ubos-webui-preact-dist";
  version = "0.1.0";

  # Only the inputs the build reads; excludes node_modules, dist, and
  # the Rust side so a stray local build does not perturb the hash.
  src = lib.cleanSourceWith {
    src = ../ubos/ubos-webui-preact;
    filter =
      path: _type:
      let
        base = baseNameOf path;
      in
      base != "node_modules" && base != "dist" && base != "target";
  };

  # See the header for how to regenerate this when package-lock.json
  # moves.
  npmDepsHash = "sha256-wdQXDZgRUEexyjuhLx1JS724mk4Susy7J2ULmD7dwX4=";

  # build.rs and rsbuild.config.ts read these for the footer; bake them
  # in here so the hermetic dist is stamped with the source revision.
  inherit (webuiBuildEnv) UBOS_WEBUI_BUILD_REV UBOS_WEBUI_BUILD_TIME;

  # Gate the strict TypeScript types before the bundle is built, so a
  # type error fails the hermetic build here rather than reaching a
  # browser. `tsc --noEmit` reads tsconfig.json and emits nothing.
  preBuild = ''
    npm run typecheck
  '';

  # `npm run build` (rsbuild) emits the two pages into dist/.
  # There is no npm package to install, so copy the pages to $out.
  dontNpmInstall = true;
  installPhase = ''
    runHook preInstall
    mkdir -p $out
    cp dist/index.html dist/login.html $out/
    runHook postInstall
  '';
}
