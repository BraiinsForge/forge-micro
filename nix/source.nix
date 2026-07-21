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

# Cleaned source trees for the crane builds: the embedded workspace
# and the out-of-tree load-test crate.
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
{ pkgs, craneLib }:
let
  inherit (pkgs) lib;
  root = ../.;
  crateGraph = import ./crate-graph.nix { inherit lib root; };

  # Crane's default filter keeps Cargo.toml, Cargo.lock, and .rs files.
  # We also need .cargo/config.toml, which holds per-crate target
  # settings, `.x` linker scripts, the vendored embassy-net-wiznet
  # patches directory, the dashboard HTML/CSS/JS that ubos-webui's
  # build.rs composes and gzips into OUT_DIR, the Braiins wordmark
  # served at /logo.svg via include_bytes!, the build.rs sibling at
  # the ubos-webui crate root, the stratum-v2 golden transcript
  # fixtures that the replay tests pull in via include_bytes!, and the
  # ubos-snapshot JSON fixture its report tests pull in via
  # include_str!.
  customFilter =
    path: _type:
    (builtins.match ".*\\.cargo/config\\.toml$" path != null)
    || (builtins.match ".*\\.x$" path != null)
    || (builtins.match ".*/patches/embassy-net-wiznet/.*" path != null)
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
  # config, and the `[patch]` override crates embassy-net-wiznet and
  # edge-nal-embassy that cargo must resolve even when the crate under
  # build does not use them. `cleanSourceWith` hashes only these files,
  # so an edit outside the closure leaves the result unchanged.
  scopedFilter =
    dirs: path: type:
    let
      rel = relOf path;
      # `rel` is one of the closure dirs, inside one, or an ancestor of
      # one, so cleanSource descends into it to reach the wanted subdir.
      inClosure = builtins.any (
        d: rel == d || lib.hasPrefix (d + "/") rel || lib.hasPrefix (rel + "/") (d + "/")
      ) dirs;
      shared =
        rel == "Cargo.toml"
        || rel == "Cargo.lock"
        || rel == ".cargo"
        || lib.hasPrefix ".cargo/" rel
        || rel == "patches"
        || lib.hasPrefix "patches/" rel;
    in
    (shared || inClosure) && admit path type;

  # The always-shared roots to overlay onto the dummy workspace, on top of
  # the closure directories. The workspace manifest and lock are overlaid
  # too so the scoped source carries the real ones verbatim.
  sharedRoots = [
    "Cargo.toml"
    "Cargo.lock"
    ".cargo"
    "patches"
  ];

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
    '';
in
{
  inherit crateGraph;

  src = lib.cleanSourceWith {
    src = root;
    filter = admit;
  };

  # Source scoped to the transitive closure of the named cargo packages.
  srcFor = names: mkScoped (crateGraph.closureForNames names);

  # Host-target load-test crate lives outside the ubos-main
  # workspace, so it gets its own src filter and dep cache rather
  # than sharing the embedded workspace wiring above.
  loadTestSrc = craneLib.cleanCargoSource (root + "/nuc/pkgs/load-test");
}
