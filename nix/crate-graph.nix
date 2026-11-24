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

# Transitive workspace path-dependency closure for every crate, derived
# from the Cargo manifests. A crane derivation given a source scoped to
# its crate's closure keeps its input hash stable when files outside the
# closure change, so nix reuses the cached build instead of recompiling.
#
# The closure is authoritative for code dependencies because it walks the
# same `path`/`workspace = true` graph cargo resolves. It is a superset of
# every file a build reads only while no crate reaches outside its own
# directory for a non-cargo dependency, such as an `include_bytes!`/
# `include_str!` or `build.rs` read with a `..` path. Nothing enforces
# that invariant directly, but a violation is not silent: the offending
# crate's own scoped build in nix/checks.nix fails, because the file it
# reaches for is absent from its scoped source.
#
# See nix/source.nix for how the closure is turned into a scoped source.
{ lib, root }:
let
  readManifest = dir: builtins.fromTOML (builtins.readFile (root + "/${dir}/Cargo.toml"));
  rootManifest = builtins.fromTOML (builtins.readFile (root + "/Cargo.toml"));

  memberDirs = rootManifest.workspace.members;
  wsDeps = rootManifest.workspace.dependencies or { };

  # Canonicalise a repo-relative directory built from `base` plus a
  # possibly `.`/`..`-laden `rel`, collapsing the parent segments.
  normalise =
    base: rel:
    let
      parts = builtins.filter (p: p != "" && p != ".") (
        lib.splitString "/" base ++ lib.splitString "/" rel
      );
      step = acc: p: if p == ".." then lib.init acc else acc ++ [ p ];
    in
    builtins.concatStringsSep "/" (builtins.foldl' step [ ] parts);

  # Resolve one dependency entry to a repo-relative crate directory, or
  # null when it is an external crates.io or git dependency with no path.
  resolveDep =
    crateDir: name: spec:
    if !builtins.isAttrs spec then
      null # bare version string
    else if spec ? path then
      normalise crateDir spec.path
    else if (spec.workspace or false) then
      let
        wd = wsDeps.${spec.package or name} or null;
      in
      if wd != null && wd ? path then normalise "" wd.path else null
    else
      null;

  # Every dependency table cargo would consider for a build, including
  # dev/build deps and target-conditional tables.
  depTables =
    m:
    [
      (m.dependencies or { })
      (m.dev-dependencies or { })
      (m.build-dependencies or { })
    ]
    ++ builtins.concatMap (t: [
      (t.dependencies or { })
      (t.dev-dependencies or { })
      (t.build-dependencies or { })
    ]) (builtins.attrValues (m.target or { }));

  directDeps =
    crateDir:
    let
      specs = builtins.concatMap (tbl: lib.mapAttrsToList (n: s: resolveDep crateDir n s) tbl) (
        depTables (readManifest crateDir)
      );
    in
    builtins.filter (d: d != null) specs;

  # Transitive closure of directories reachable from a crate's own dir.
  closureDirs =
    crateDir:
    map (x: x.key) (
      builtins.genericClosure {
        startSet = [ { key = crateDir; } ];
        operator = item: map (d: { key = d; }) (directDeps item.key);
      }
    );

  # Package name -> its crate directory, for callers that key by the
  # cargo package name, as the checks tables do.
  dirForName = builtins.listToAttrs (
    map (dir: {
      name = (readManifest dir).package.name;
      value = dir;
    }) memberDirs
  );

  # Crate directory -> its package name, the reverse of dirForName.
  nameForDir = lib.mapAttrs' (name: dir: lib.nameValuePair dir name) dirForName;
in
{
  inherit memberDirs dirForName;

  # The workspace packages a package lists itself, in any dependency
  # table, by name. nix/crate-layers.nix holds the base crates to an
  # allowed list with it.
  directDepNames =
    name:
    lib.unique (
      map (dir: nameForDir.${dir}) (
        builtins.filter (dir: nameForDir ? ${dir}) (directDeps dirForName.${name})
      )
    );

  # All workspace package names.
  members = builtins.attrNames dirForName;

  # Union of closures for several package names, deduplicated.
  closureForNames =
    names: lib.unique (builtins.concatMap closureDirs (map (n: dirForName.${n}) names));
}
