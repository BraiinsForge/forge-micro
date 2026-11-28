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

# The two gates a board table owes its boards: that every board is
# gated at all, and that its brand is declared the same way twice.
#
# The brand a board ships is stated twice, and it has to be. A board
# declares it in its own `.cargo/config.toml`, which is what an
# interactive build and a bench flash read; nix/brands.nix declares it
# for every nix build, because crane runs cargo from the flake root and
# reaches no crate's config from there. Neither file can import the
# other, so this gate is the agreement between them, in the shape
# named-constants.md prescribes for a value two languages share.
#
# The fault it catches ships a working page. Every build succeeds and
# every page is correct, with the wrong words on it, so no build gate
# bites. A grep of the built image cannot stand in either: the pages are
# Zopfli-gzipped into the binary, so the brand name is not a string in
# it.
#
# Both trees of the pair gate this, against the same brand directories,
# so the check lives here and each tree calls it rather than keeping a
# copy. `board-parity` and `guards` are shared the same way and for the
# same reason: two copies of one rule drift, and nothing compares them.
#
# `boardCoverage` is here for that reason and no other. It has nothing
# to do with brands, but it is one rule with one list per tree, the
# same shape as `brandDeclaration`, and the two trees had already grown
# two copies of it that disagreed.
{
  lib,
  pkgs,
  # The tree whose board configs are read. Each tree passes its own.
  root,
  # nix/brands.nix instantiated for this tree, carrying its prefix.
  brands,
  # Whether to gate each brand directory for completeness. The tree that
  # holds the directories does it; the other reaches the same files
  # through the submodule and would report every fault twice.
  checkBrandDirs ? true,
}:
let
  # The keys every brand.json must carry; ubos-webui's build.rs reads
  # each one and fails without it. This gate reads the file before a
  # build gets that far.
  brandKeys = [
    "name"
    "accent"
    "accentHi"
    "accentText"
    "icon"
  ];

  # build.rs takes the icon's MIME type from its extension and knows
  # these two.
  brandIconPattern = "favicon\\.(png|svg)";

  # The form each value must take. `name` reaches a <title> and an alt
  # attribute, and the three colours reach a style block in the same
  # head, all by plain substitution in build.rs. Nothing escapes them on
  # the way, so a value carrying markup or a stray quote would not be
  # shown on the page, it would be parsed as part of it. A colour that
  # is merely misspelt is the quieter half of the same fault: the
  # browser discards the declaration and the page loses its accent with
  # nothing failing. These two patterns are what keeps either off a
  # board.
  brandColourKeys = [
    "accent"
    "accentHi"
    "accentText"
  ];
  brandColourPattern = "#[0-9a-fA-F]{6}";
  brandNamePattern = "[^\"'<>&]+";

  # How this tree spells the two files the gate names in a fault. A
  # consumer reaching the brand through a submodule carries the prefix
  # on both, so the message points at a path that tree actually has.
  tablePath = "${brands.prefix}nix/brands.nix";
  readmePath = "${brands.prefix}brand/README.md";

  # A value is well formed when it is a string matching the pattern its
  # key requires. A key that is absent is reported as missing rather
  # than as malformed, so each fault is named once.
  malformed =
    brand: key: pattern:
    (brand ? ${key})
    && !(builtins.isString brand.${key} && builtins.match pattern brand.${key} != null);

  # `relative = true` resolves a board's value against its own crate
  # directory, so a declaration is `../../<brand dir>`. Compare the
  # directory itself, not the path each site spells to reach it.
  declaredBy =
    board:
    let
      config = builtins.fromTOML (builtins.readFile (root + "/firmware/${board}/.cargo/config.toml"));
    in
    lib.removePrefix "../../" config.env.UBOS_BRAND.value;

  incompleteDirs = lib.concatMap (
    dir:
    let
      brand = builtins.fromJSON (builtins.readFile (root + "/${dir}/brand.json"));
    in
    lib.optional (
      !builtins.pathExists (root + "/${dir}/dashboard-logo.svg")
    ) "${dir} has no dashboard-logo.svg, which is the wordmark the dashboard serves"
    ++ lib.optional (
      !builtins.pathExists (root + "/${dir}/login-logo.svg")
    ) "${dir} has no login-logo.svg, which is the wordmark the login page serves"
    ++ lib.optional (malformed brand "icon" brandIconPattern) (
      "${dir}/brand.json: `icon` is neither favicon.png nor favicon.svg; "
      + "build.rs reads the icon's type from its extension"
    )
    ++ lib.optional (
      brand ? icon
      && !(malformed brand "icon" brandIconPattern)
      && !builtins.pathExists (root + "/${dir}/${brand.icon}")
    ) "${dir} has no ${brand.icon}, which brand.json names as the tab icon every page links"
    ++ map (key: "${dir}/brand.json has no `${key}` key, which the pages need") (
      lib.filter (key: !(brand ? ${key})) brandKeys
    )
    ++ map (
      key:
      "${dir}/brand.json: `${key}` is not ${brandColourPattern}; "
      + "the pages put it into a style block unquoted"
    ) (lib.filter (key: malformed brand key brandColourPattern) brandColourKeys)
    ++ lib.optional (malformed brand "name" brandNamePattern) (
      "${dir}/brand.json: `name` carries a quote or a markup character; "
      + "the pages put it into a title and an alt attribute unescaped"
    )
  ) brands.allDirs;
in
{
  # Every firmware directory is named by a gate, and every gate names a
  # firmware directory.
  #
  # A tree's check table keys on the crate name, so a row naming a
  # crate that is not there is dropped rather than reported. Rename a
  # board's directory and its package without renaming its row and the
  # board loses `clippy-`, `build-`, its slot guard and its brand row,
  # while `nix flake check` stays green. That is the failure this gate
  # exists for, and a rename is only the commonest way to reach it: a
  # board added with no row fails the same way and reports the same
  # nothing. The gate compares names and nothing else, so a stale
  # spelling in prose, in CI or in an identifier is not its to catch.
  #
  # Both directions are checked, because each catches the half the
  # other cannot. A directory with no gate is the silent case above. A
  # gate with no directory is the stale row left behind by the same
  # edit, harmless today and the first case as soon as somebody reuses
  # the name. The stale direction is never filtered: a directory that
  # is gone is not a workspace member either, so filtering by
  # membership would drop exactly the row it is there to report.
  #
  # `boards` is every name a gate in the calling tree names. `isMember`
  # says which firmware directories the tree builds; a directory it
  # rejects is not a member of the workspace, so no gate is owed for it.
  # That is what lets a tree carrying a subset of its boards evaluate.
  boardCoverage =
    {
      boards,
      isMember ? (_: true),
    }:
    let
      firmwareDirs = builtins.attrNames (
        lib.filterAttrs (_: kind: kind == "directory") (builtins.readDir (root + "/firmware"))
      );

      ungated = lib.filter (name: isMember name && !(builtins.elem name boards)) firmwareDirs;

      stale = lib.filter (name: !(builtins.elem name firmwareDirs)) boards;

      faults =
        map (name: "firmware/${name} is built here and no check names it") ungated
        ++ map (name: "a check names '${name}' and firmware/${name} does not exist") stale;
    in
    pkgs.runCommand "board-coverage" { } (
      lib.concatMapStrings (fault: ''
        echo ${lib.escapeShellArg "ERROR: ${fault}."} >&2
      '') faults
      + lib.optionalString (faults != [ ]) ''
        echo "A board is gated by the check that names it. A board named" >&2
        echo "by no check is not gated at all, and nothing else reports that." >&2
        exit 1
      ''
      + lib.concatMapStrings (name: ''
        echo ${lib.escapeShellArg name}
      '') boards
      + ''
        touch $out
      ''
    );

  # `boards` is every board in the calling tree whose own
  # `.cargo/config.toml` declares a brand. A list rather than the
  # caller's own board table, because the two trees keep that table in
  # different shapes and only the names are shared.
  brandDeclaration =
    { boards }:
    let
      mismatched = lib.filter (board: declaredBy board != brands.dirFor board) boards;

      faults =
        map (
          board:
          "${board} declares '${declaredBy board}' in firmware/${board}/.cargo/config.toml, "
          + "but ${tablePath} gives it '${brands.dirFor board}'"
        ) mismatched
        ++ lib.optionals checkBrandDirs incompleteDirs;
    in
    # Every fault and every brand name goes through escapeShellArg. Both
    # carry backticks, and a name is read from a file, so interpolating
    # either one bare puts a command substitution in the script: the
    # message then loses the word it was naming and the shell reports a
    # missing command beside it.
    pkgs.runCommand "brand-declaration" { } (
      lib.concatMapStrings (fault: ''
        echo ${lib.escapeShellArg "ERROR: ${fault}."} >&2
      '') faults
      + lib.optionalString (faults != [ ]) ''
        echo ${lib.escapeShellArg "${readmePath} holds the board table and the file format."} >&2
        exit 1
      ''
      + lib.concatMapStrings (board: ''
        echo ${lib.escapeShellArg "${board}: ${brands.nameFor board}"}
      '') boards
      + ''
        touch $out
      ''
    );
}
