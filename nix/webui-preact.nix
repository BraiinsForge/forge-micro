# Hermetic Rsbuild/Preact build for `ubos/ubos-webui-preact`.
#
# This produces the two single-file pages the crate's build.rs embeds:
# `$out/index.html` and `$out/login.html`. Point the crate's build at it
# with `UBOS_WEBUI_PREACT_DIST = "${webuiPreactDist}"` so the Rust check
# sandbox never needs Node — build.rs just gzips these two files.
#
# STATUS: not yet imported by flake.nix. Wiring it in is a deliberate,
# reviewed step (see "Wiring" below) because `nix flake check` cannot
# pass until `npmDepsHash` is filled with the real value — which needs
# a network fetch that only runs under Nix.
#
# One-time hash:
#   nix run nixpkgs#prefetch-npm-deps -- \
#     ubos/ubos-webui-preact/package-lock.json
# Paste the printed hash into `npmDepsHash` below (or leave `fakeHash`
# and copy the "got:" hash from the first build's error).
#
# Wiring (after the hash is set):
#   1. flake.nix: build this in the per-system `let` block and thread it
#      into `nix/lib.nix` helpers, e.g.
#        webuiPreactDist = pkgs.callPackage ./nix/webui-preact.nix {
#          inherit webuiBuildEnv;
#        };
#      then expose `preactDistEnv = { UBOS_WEBUI_PREACT_DIST =
#      "${webuiPreactDist}"; };` from lib.nix alongside `webuiEnvAttrs`.
#   2. nix/checks.nix: add a `preactDist` spec flag to mkHostClippy /
#      mkHostTest that merges `preactDistEnv`, then register:
#        clippy-ubos-webui-preact = { crate = "ubos-webui-preact";
#          preactDist = true; webuiEnv = true; };
#        test-ubos-webui-preact   = { crate = "ubos-webui-preact";
#          preactDist = true; webuiEnv = true; };
#      Add both to the `web-host` aggregate (and keep them out of
#      `fast-host`, like the ubos-webui checks).
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

  # Placeholder — replace with the real hash (see header). Kept as
  # fakeHash so an accidental import fails loudly at build rather than
  # silently building stale assets.
  npmDepsHash = lib.fakeHash;

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
