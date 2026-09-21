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

# Cleaned source trees for the crane builds.
#
# `src` is the whole cleaned workspace, used by the workspace-wide gates,
# such as fmt and cargo-machete, that legitimately read every crate.
# `srcFor names` returns a source in which only the named crates'
# transitive path-dependency closure carries real code; see
# nix/crate-graph.nix. Every other workspace member is reduced to a crane
# dummy so the virtual workspace still loads. Its two inputs — the
# all-dummy base, manifests only, and the real closure overlay, closure
# plus shared roots only — each ignore every file outside their scope,
# so an edit to an unrelated crate leaves the scoped source's hash
# unchanged and nix reuses the cached build instead of recompiling.
# The per-crate checks and images consume `srcFor`.
#
# `root` defaults to this repo's tree. A downstream repo consuming this
# module through the flake's `lib.modules` passes its own tree instead:
# the crate graph and every scoped source are then read from there, so
# the module works unchanged on a workspace that is not this one.
{
  pkgs,
  craneLib,
  patchedCrates,
  root ? ../.,
  # Directory the workspace `[patch]` table points at, relative to
  # `root`. Same argument, and the same reason, as in nix/lib.nix: a
  # consumer reaching these crates through a submodule carries a prefix
  # on its `[patch]` paths, and an overlay writing to the unprefixed
  # path would put the trees where cargo does not look.
  patchesDir ? "patches",
  # Extra `root`-relative paths to carry verbatim into every scoped
  # source, alongside the workspace manifest, lock and `.cargo` config.
  #
  # This exists for a consumer whose closure reaches crates belonging to
  # a *nested* workspace, which is what consuming this repo through a
  # submodule means. `mkDummySrc` reserializes every manifest it finds,
  # and crane's cleaner drops `[workspace.lints]` on the way; the outer
  # root manifest survives because it is overlaid back verbatim, but a
  # nested one is not in that list, so its members fail to load with
  # "error inheriting `lints` from workspace root manifest". Naming the
  # nested root here overlays it back the same way.
  extraRoots ? [ ],
}:
let
  inherit (pkgs) lib;
  crateGraph = import ./crate-graph.nix { inherit lib root; };

  # The pinned-and-patched upstream crates from nix/patched-crates.nix
  # live in the store, not the repo, so every source the cargo builds
  # consume gets them copied in at ${patchesDir}/<crate>, where the
  # workspace `[patch]` table points.
  patchedCratesOverlay = lib.concatStrings (
    lib.mapAttrsToList (name: drv: ''
      rm -rf "$out/${patchesDir}/${name}"
      mkdir -p "$out/${patchesDir}"
      cp -a --no-preserve=mode ${drv} "$out/${patchesDir}/${name}"
    '') patchedCrates
  );

  # Crane's default filter keeps Cargo.toml, Cargo.lock, and .rs files.
  # We also need .cargo/config.toml, which holds per-crate target
  # settings, `.x` linker scripts, the two single-file HTML pages that
  # ubos-webui's build.rs reads and gzips into OUT_DIR, the icon set the
  # frontend imports, the build.rs sibling at the ubos-webui crate root,
  # the stratum-v2 golden transcript fixtures that the replay tests pull
  # in via include_bytes!, and the ubos-snapshot JSON fixture its report
  # tests pull in via include_str!.
  #
  # The brand directories are deliberately absent. Nothing in a nix build
  # reads a brand out of this source: `brands.envFor` in nix/brands.nix
  # interpolates the brand directory as a store path of its own and
  # passes it as UBOS_BRAND, so a build depends on the one brand it ships
  # and the wordmark never enters a crate's source. Admitting the
  # directory here would put every brand in every scoped source below,
  # and an edit to one wordmark would then rebuild every crate.
  customFilter =
    path: _type:
    (builtins.match ".*\\.cargo/config\\.toml$" path != null)
    || (builtins.match ".*\\.x$" path != null)
    || (builtins.match ".*\\.html$" path != null)
    || (builtins.match ".*\\.css$" path != null)
    || (builtins.match ".*\\.js$" path != null)
    || (builtins.match ".*\\.svg$" path != null)
    || (builtins.match ".*/tests/fixtures/.*\\.bin$" path != null)
    || (builtins.match ".*/tests/fixtures/.*\\.json$" path != null)
    || (baseNameOf path == "build.rs");

  # The top-level nix/ directory holds only flake modules; crane's
  # filter admits every directory, so without this exclusion it
  # would land in the cleaned source as an empty directory and
  # churn the src hash on every flake refactor.
  admit =
    path: type:
    (path != toString (root + "/nix"))
    && ((customFilter path type) || (craneLib.filterCargoSources path type));

  # Manifest-only source: every Cargo.toml plus the workspace lockfile,
  # nothing else. `cleanSourceWith` hashes solely on the files the filter
  # admits, so a source edit that leaves the manifests untouched does not
  # perturb it. `mkDummySrc` turns it into a workspace of real manifests
  # with stub targets, which is what lets cargo load the whole virtual
  # workspace from a source that carries only some crates' real code:
  # cargo reads every member in `workspace.members` even when building one
  # crate with `-p`, and rejects a member whose target files are missing.
  manifestFilter =
    path: type:
    type == "directory"
    || (
      let
        base = baseNameOf path;
      in
      base == "Cargo.toml" || base == "Cargo.lock"
    );
  manifestsSrc = lib.cleanSourceWith {
    src = root;
    filter = manifestFilter;
  };
  dummyWorkspace = craneLib.mkDummySrc { src = manifestsSrc; };

  rootStr = toString root;
  relOf = path: lib.removePrefix (rootStr + "/") (toString path);

  # A scoped real source admits only files under the closure directories
  # plus the always-shared roots: workspace manifest/lock, `.cargo`
  # config, and whatever `extraRoots` a consumer adds; the `[patch]`
  # override crates cargo must resolve even when the crate under build
  # does not use them arrive via patchedCratesOverlay.
  # `cleanSourceWith` hashes only these files, so an edit outside the
  # closure leaves the result unchanged.
  scopedFilter =
    dirs: path: type:
    let
      rel = relOf path;
      # Both tests admit three cases: `rel` is the entry itself, inside
      # it, or an ancestor of it. The ancestor case is what makes
      # cleanSource descend far enough to reach the entry at all, so a
      # nested `extraRoots` path such as `<submodule>/Cargo.toml` needs
      # it as much as a closure dir does; without it that path is
      # dropped silently and the overlay below skips it.
      reaches =
        entry: rel == entry || lib.hasPrefix (entry + "/") rel || lib.hasPrefix (rel + "/") (entry + "/");
      inClosure = builtins.any reaches dirs;
      shared = builtins.any reaches sharedRoots;
    in
    (shared || inClosure) && admit path type;

  # The always-shared roots to overlay onto the dummy workspace, on top of
  # the closure directories. The workspace manifest and lock are overlaid
  # too so the scoped source carries the real ones verbatim; `extraRoots`
  # extends the list for a consumer whose own workspace root sits at a
  # path this repo does not know.
  #
  # `brand` does not belong here, however much it looks like it should.
  # It sits at the tree root and belongs to no crate, but ubos-webui's
  # build.rs reads it through UBOS_BRAND, which nix/brands.nix sets to a
  # store path of the one brand that board ships. Adding it would put
  # every brand in every crate's scoped source and make a wordmark edit
  # rebuild the whole workspace, which is the opposite of what this list
  # is for.
  sharedRoots = [
    "Cargo.toml"
    "Cargo.lock"
    ".cargo"
  ]
  ++ extraRoots;

  # Overlay the real closure directories, and the shared roots, from a
  # closure-scoped source onto the all-dummy workspace, so only those
  # directories carry real code. The scoped build then depends on just
  # two inputs — the manifest-only dummy base and this closure-scoped
  # overlay — neither of which changes when an unrelated crate is edited.
  mkScoped =
    dirs:
    let
      real = lib.cleanSourceWith {
        src = root;
        filter = scopedFilter dirs;
      };
      overlay = lib.concatMapStringsSep "\n" (d: ''
        if [ -e "${real}/${d}" ]; then
          rm -rf "$out/${d}"
          mkdir -p "$(dirname "$out/${d}")"
          cp -a --no-preserve=mode "${real}/${d}" "$out/${d}"
        fi
      '') (sharedRoots ++ dirs);
    in
    pkgs.runCommand "ubos-src-scoped" { } ''
      mkdir -p "$out"
      cp -a --no-preserve=mode ${dummyWorkspace}/. "$out/"
      ${overlay}
      ${patchedCratesOverlay}
    '';

  cleanedSrc = lib.cleanSourceWith {
    src = root;
    filter = admit;
  };
in
{
  inherit crateGraph;

  src = pkgs.runCommand "ubos-src" { } ''
    cp -a --no-preserve=mode ${cleanedSrc} "$out"
    ${patchedCratesOverlay}
  '';

  # Source scoped to the transitive closure of the named cargo packages.
  srcFor = names: mkScoped (crateGraph.closureForNames names);

}
