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

# Every cargo unit as one derivation, through rostnix, for the host
# checks and the ARM board checks and images.
#
# Cargo plans during evaluation and rustc builds one unit per
# derivation, so an edit rebuilds only the units that read the file and
# their dependents. The features, profile, link arguments and lints are
# read from the manifests, so a check builds what `cargo build` and
# `cargo test` would.
#
# Every unit compiles through `clippyRustc`, so a build is its own lint
# and one graph serves the clippy, test and build checks of a crate, and
# an image.
{
  pkgs,
  # For the cross package set each board target builds in.
  nixpkgs,
  # Patched so a selection that installs nothing, a library crate's
  # units or its tests, plans and builds, so an override adds rustc
  # flags for its own package, and so a test run keeps the coverage
  # profiles it writes.
  rostnix,
  src,
  hostRustToolchain,
  rustToolchain,
  # See flake.nix.
  webuiHostEnv,
  webuiEnv,
  # Board targets beyond the ARM ones below, by triple, each written as
  # the ARM entries are.
  extraTargets ? { },
}:
let
  inherit (pkgs) lib;

  rostnixLib = import (
    pkgs.applyPatches {
      name = "rostnix-source";
      src = rostnix;
      patches = [
        ./rostnix-libraries.patch
        ./rostnix-rustflags.patch
        ./rostnix-profiles.patch
      ];
    }
  );

  # The rustc that runs clippy on the workspace's own packages, with
  # warnings denied, then builds the unit itself: clippy-driver compiles
  # with other MIR settings, and an image has to be what `cargo build`
  # makes. The vendored trees under `patches/` and `micro-sv2/`, which
  # its own flake lints, are not linted. clippy.toml sits in a directory
  # of its own, because a unit's source view holds its package alone.
  clippyConf = pkgs.writeTextDir "clippy.toml" (builtins.readFile "${src}/clippy.toml");
  clippyRustc =
    toolchain:
    pkgs.symlinkJoin {
      name = "rust-clippy-driver";
      paths = [ toolchain ];
      postBuild = ''
        rm "$out/bin/rustc"
        cat > "$out/bin/rustc" <<'EOS'
        #!${pkgs.runtimeShell}
        set -e
        real=${toolchain}/bin/rustc
        case " $* " in *" --cap-lints allow "*) exec "$real" "$@" ;; esac
        case "$CARGO_MANIFEST_DIR" in */patches/* | */micro-sv2/*) exec "$real" "$@" ;; esac
        [ -n "$CARGO_CRATE_NAME" ] || exec "$real" "$@"
        lint=$(${pkgs.coreutils}/bin/mktemp -d)
        args=()
        for arg in "$@"; do
          case "$prev $arg" in
            "--out-dir "*) arg=$lint ;;
            *" --emit=link") arg=--emit=metadata ;;
          esac
          args+=("$arg")
          prev=$arg
        done
        CLIPPY_CONF_DIR=${clippyConf} ${toolchain}/bin/clippy-driver "''${args[@]}" -D warnings
        ${pkgs.coreutils}/bin/rm -rf "$lint"
        exec "$real" "$@"
        EOS
        chmod +x "$out/bin/rustc"
      '';
    };

  # The toolchain is also the cargo that plans.
  mkEnv =
    {
      pkgs,
      toolchain,
      linker ? null,
    }:
    rostnixLib {
      inherit pkgs linker;
      rustc = clippyRustc toolchain;
      cargo = toolchain;
    };

  host = mkEnv {
    inherit pkgs;
    toolchain = hostRustToolchain;
  };

  # The cross package set's platform is the triple, so build scripts and
  # proc macros build for this machine and everything else for the
  # board. The C of secp256k1-sys, which every board reaches through
  # ii-sv2, compiles with the prebuilt arm-none-eabi-gcc, because the
  # cross package set's gcc adds hardening flags Thumb-1 does not
  # implement.
  rustLld = "${rustToolchain}/lib/rustlib/${pkgs.stdenv.hostPlatform.rust.rustcTarget}/bin/rust-lld";
  ccVar = triple: "CC_${builtins.replaceStrings [ "-" "." ] [ "_" "_" ] triple}";
  armTarget = triple: base: {
    inherit base;
    toolchain = rustToolchain;
    linker = rustLld;
    crateOverrides.secp256k1-sys = {
      nativeBuildInputs = [ pkgs.gcc-arm-embedded ];
      env.${ccVar triple} = "${pkgs.gcc-arm-embedded}/bin/arm-none-eabi-gcc";
      optional = true;
    };
  };
  targetSpecs = {
    "thumbv8m.main-none-eabihf" =
      armTarget "thumbv8m.main-none-eabihf" lib.systems.examples.armhf-embedded;
    "thumbv6m-none-eabi" = armTarget "thumbv6m-none-eabi" lib.systems.examples.arm-embedded;
  }
  // extraTargets;
  targets = lib.mapAttrs (
    triple: spec:
    spec
    // {
      env = mkEnv {
        pkgs = import nixpkgs {
          inherit (pkgs.stdenv.hostPlatform) system;
          crossSystem = spec.base // {
            rust.rustcTarget = triple;
          };
        };
        inherit (spec) toolchain linker;
      };
    }
  ) targetSpecs;

  packageOf =
    plan: name:
    lib.findFirst (p: p.name == name) (throw "units: ${name} is not a package of the plan ${plan.name}")
      (lib.attrValues plan.graph.packages);

  # A plan keys each unit by its package, the version and the unit.
  unitsOf =
    plan: name:
    let
      package = packageOf plan name;
      prefix = "${package.name}-${package.version}-";
    in
    lib.attrValues (lib.filterAttrs (key: _: lib.hasPrefix prefix key) plan.graph.units);

  # ubos-config reads `DEVFEE` through `option_env!`, so the fee switch
  # has to reach that crate's compile.
  feeOverride = fee: {
    ubos-config = {
      env = fee;
      optional = true;
    };
  };

  testsOf =
    plan: name: lib.attrValues (lib.filterAttrs (_: test: test.packageName == name) plan.tests);
in
{
  # A plan over one host crate, as `cargo test -p` plans it, in the dev
  # profile so debug_assert! and overflow checks fire. One crate per
  # plan, because cargo unifies the features of what it plans together,
  # and the host crates disagree on them.
  hostPlan =
    {
      crate,
      features ? [ ],
      noDefaultFeatures ? false,
      tests ? true,
      checkFlags ? [ ],
      fee ? { },
    }:
    host.buildRustApplication {
      pname = crate;
      inherit
        src
        features
        noDefaultFeatures
        checkFlags
        ;
      packages = [ crate ];
      profile = "dev";
      doCheck = tests;
      crateOverrides = feeOverride fee // {
        ubos-webui = {
          env = webuiHostEnv;
          optional = true;
        };
      };
    };

  # One crate's release build for a board target, with the image at
  # `bin/<name>` when the crate is a board. `workspaceEnv` is the `[env]`
  # a board's own `.cargo/config.toml` states, which cargo reads only
  # from the workspace root it plans from, so the plan's source carries
  # it there.
  boardPlan =
    {
      name,
      pname ? name,
      target,
      features ? [ ],
      noDefaultFeatures ? false,
      env ? { },
      workspaceEnv ? { },
      fee ? { },
    }:
    let
      spec = targets.${target};
      planSrc =
        if workspaceEnv == { } then
          src
        else
          pkgs.runCommand "${src.name}-${name}" { } ''
            cp -r --no-preserve=mode ${src} "$out"
            printf '\n[env]\n' >> "$out/.cargo/config.toml"
            ${lib.concatStrings (
              lib.mapAttrsToList (k: v: ''
                printf '%s = "%s"\n' ${lib.escapeShellArg k} ${lib.escapeShellArg v} >> "$out/.cargo/config.toml"
              '') workspaceEnv
            )}
          '';
    in
    spec.env.buildRustApplication {
      inherit pname features noDefaultFeatures;
      src = planSrc;
      packages = [ name ];
      doCheck = false;
      crateOverrides =
        spec.crateOverrides or { }
        // feeOverride fee
        // {
          ubos-webui = {
            env = webuiEnv name;
            optional = true;
          };
          ${name} = {
            inherit env;
            nativeBuildInputs = spec.nativeBuildInputs or [ ];
          };
        };
    };

  inherit unitsOf testsOf;
}
