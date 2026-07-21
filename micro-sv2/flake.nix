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

{
  description = "micro-sv2 — compact no_std Stratum V2 implementation";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.11";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    crane.url = "github:ipetkov/crane";
    treefmt-nix = {
      url = "github:numtide/treefmt-nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
      rust-overlay,
      crane,
      treefmt-nix,
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs { inherit system overlays; };

        rustToolchain = pkgs.rust-bin.stable.latest.default.override {
          extensions = [
            "rust-src"
            "rust-analyzer"
            "rustfmt"
            "clippy"
          ];
        };

        craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;

        # Cargo-relevant files only, with interop-test/ excluded. interop-test
        # sits in the tree but is outside the workspace (proprietary registry
        # dep — see the root Cargo.toml `exclude`).
        src = pkgs.lib.cleanSourceWith {
          src = ./.;
          filter =
            path: type:
            (craneLib.filterCargoSources path type) && (builtins.match ".*/interop-test(/.*)?$" path == null);
          name = "source";
        };

        # `pname`/`version` are needed because the root Cargo.toml is a pure
        # workspace manifest with no [package] section — crane would otherwise
        # warn and fall back to a placeholder. Member crates carry their own
        # versions; this is just the umbrella label for crane derivations.
        commonArgs = {
          inherit src;
          strictDeps = true;
          pname = "micro-sv2";
          version = "0.2.0";
        };

        cargoArtifacts = craneLib.buildDepsOnly commonArgs;

        treefmtEval = treefmt-nix.lib.evalModule pkgs {
          projectRootFile = "flake.nix";
          programs.nixfmt.enable = true;
          settings.formatter.rustfmt = {
            command = "${rustToolchain}/bin/rustfmt";
            options = [
              "--config"
              "skip_children=true"
              "--edition"
              "2024"
            ];
            includes = [ "**/*.rs" ];
            excludes = [ "interop-test/**" ];
          };
        };
      in
      {
        formatter = treefmtEval.config.build.wrapper;

        checks = {
          clippy = craneLib.cargoClippy (
            commonArgs
            // {
              inherit cargoArtifacts;
              cargoClippyExtraArgs = "--workspace --all-targets -- -D warnings";
            }
          );
          test = craneLib.cargoTest (
            commonArgs
            // {
              inherit cargoArtifacts;
              cargoTestExtraArgs = "--workspace";
            }
          );
          formatting = treefmtEval.config.build.check self;
        };

        devShells.default = pkgs.mkShell {
          packages = [ rustToolchain ];
          shellHook = ''
            echo "micro-sv2 dev environment ready"
            echo ""
            echo "  cargo build         Build the workspace"
            echo "  cargo clippy        Lint the workspace"
            echo "  cargo test          Run workspace tests"
            echo "  nix flake check     Run all CI checks locally"
            echo "  nix fmt             Apply repo-wide formatting"
            echo ""
          '';
        };
      }
    );
}
