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

# Hermetic Rsbuild/Preact build for `ubos/ubos-webui`.
#
# This produces the two single-file pages the crate's build.rs embeds:
# `$out/index.html` and `$out/login.html`. flake.nix builds this in the
# per-system `let` block and threads it into nix/lib.nix as
# `webuiDistEnv`, which the ubos-webui checks set as
# `UBOS_WEBUI_DIST` so the Rust check sandbox never needs Node —
# build.rs gzips these two files with Zopfli.
#
# `npmDepsHash` is a hash over package-lock.json's dependency set. It
# needs a network fetch that only runs under Nix, so regenerate it
# whenever the lockfile moves:
#   nix run nixpkgs#prefetch-npm-deps -- \
#     ubos/ubos-webui/package-lock.json
# Paste the printed hash into `npmDepsHash` below. A stale hash fails
# the build and prints the correct "got:" hash to copy.
#
# The dev shells set the same variable; the header of nix/shells.nix
# says why.

{
  lib,
  buildNpmPackage,
}:

let
  inputs = [
    "web"
    "dev"
    "index.html"
    "login.html"
    "rsbuild.config.ts"
    "tsconfig.json"
    "globals.d.ts"
    "package.json"
    "package-lock.json"
  ];
in
buildNpmPackage {
  pname = "ubos-webui-dist";
  version = "0.1.0";

  # Only the inputs the build reads, the same list build.rs names in
  # its rerun-if-changed lines; the webui-inputs check holds the two
  # equal. Every shell carries this store path, so a file admitted here
  # that the build never reads would still move the path and relink
  # every firmware that embeds the pages.
  src = lib.cleanSourceWith {
    src = ../ubos/ubos-webui;
    filter =
      path: _type:
      let
        root = toString ../ubos/ubos-webui + "/";
        rel = lib.removePrefix root (toString path);
      in
      lib.any (input: rel == input || lib.hasPrefix (input + "/") rel) inputs;
  };

  passthru = { inherit inputs; };

  # See the header for how to regenerate this when package-lock.json
  # moves.
  npmDepsHash = "sha256-JNIw4Fqgp5D6SDGuHduuXDbz/9s7by//aCT3F6rU6i4=";

  # No revision or time is baked in here. The bundle carries two fixed
  # tokens for them, and the crate's build.rs stamps the pages when it
  # embeds them, so this derivation depends on the frontend sources
  # alone and one build of it serves every commit.

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
