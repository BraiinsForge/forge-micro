# Cleaned source trees for the crane builds: the embedded workspace
# and the out-of-tree load-test crate.
{ pkgs, craneLib }:
let
  root = ../.;
in
{
  src =
    let
      # Crane's default filter keeps Cargo.toml, Cargo.lock, and .rs files.
      # We also need .cargo/config.toml (per-crate target settings), linker
      # scripts (.x), the vendored embassy-net-wiznet patches directory,
      # the dashboard HTML/CSS/JS that ubos-webui's build.rs composes and
      # gzips into OUT_DIR, the Braiins wordmark served at /logo.svg via
      # include_bytes!, the build.rs sibling at the ubos-webui crate
      # root, the stratum-v2 golden transcript fixtures that the
      # replay tests pull in via include_bytes!, and the
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
    in
    pkgs.lib.cleanSourceWith {
      src = root;
      # The top-level nix/ directory holds only flake modules; crane's
      # filter admits every directory, so without this exclusion it
      # would land in the cleaned source as an empty directory and
      # churn the src hash on every flake refactor.
      filter =
        path: type:
        (path != toString (root + "/nix"))
        && ((customFilter path type) || (craneLib.filterCargoSources path type));
    };

  # Host-target load-test crate lives outside the ubos-main
  # workspace, so it gets its own src filter and dep cache rather
  # than sharing the embedded workspace wiring above.
  loadTestSrc = craneLib.cleanCargoSource (root + "/nuc/pkgs/load-test");
}
