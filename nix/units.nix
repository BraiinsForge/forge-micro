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
# derivation: a library, a binary, a test executable or a build-script
# run. An edit rebuilds the units that read the file and the units that
# depend on them, and every check shares the rest from the store. The
# features, the profile, the link arguments and the lints are cargo's
# own, read from the manifests and the workspace configuration, so a
# check builds what `cargo build` and `cargo test` would.
#
# Clippy lints the workspace's own packages, the units rostnix compiles
# without `--cap-lints allow` outside the vendored trees under
# `patches/`, which keep upstream's code, and outside `micro-sv2/`, a
# workspace of its own that its own flake lints, with warnings denied as
# `cargo clippy -- -D warnings` runs it, and rustc compiles every unit;
# `clippyRustc` below is that rule. Every plan compiles with it, so a
# build is its own lint: one graph serves the clippy, test and build
# checks of a crate, and an image.
{
  pkgs,
  # The nixpkgs source, for the cross package set each board target
  # builds in.
  nixpkgs,
  # The rostnix source, patched so a selection that installs nothing, a
  # library crate's units or its tests, plans and builds.
  rostnix,
  # The cleaned workspace, from nix/source.nix.
  src,
  hostRustToolchain,
  rustToolchain,
  # The build-script environment of ubos-webui wherever a host check
  # compiles it, and per board for an image.
  webuiHostEnv,
  webuiEnv,
  # Board targets beyond the ARM ones below, by triple, each as the
  # ARM entries are written: `base`, the nixpkgs cross system; the
  # `toolchain` that plans and compiles; the `linker`; and optionally
  # `crateOverrides` for crates that need a tool or a library, and
  # `nativeBuildInputs` for the crate under build.
  extraTargets ? { },
}:
let
  inherit (pkgs) lib;

  rostnixLib = import (
    pkgs.applyPatches {
      name = "rostnix-source";
      src = rostnix;
      patches = [ ./rostnix-libraries.patch ];
    }
  );

  # The rustc that runs clippy on the workspace's own packages. The lint
  # pass emits metadata alone, into a directory of its own, and rustc
  # then builds the unit, because clippy-driver compiles with other MIR
  # settings than rustc and an image has to be what `cargo build` makes.
  # Clippy reads the workspace's clippy.toml from a directory of its own,
  # because a unit's source view holds its package alone, and the
  # directory holds that one file so an edit elsewhere rebuilds nothing.
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

  # The build environment for one package set and toolchain. The
  # toolchain is also cargo, which plans.
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

  # A board target: the cross package set its units build in, whose
  # platform is the triple so build scripts and proc macros build for
  # this machine and everything else for the board; the toolchain that
  # plans and compiles; the linker; and what the crate under build
  # needs beyond that, as rostnix takes it. The ARM targets link with
  # rust-lld, as rustc links them on its own, and the C of
  # secp256k1-sys, which every board reaches through ii-sv2, compiles
  # with the prebuilt arm-none-eabi-gcc: the cross package set's own
  # gcc comes wrapped with hardening flags Thumb-1 does not implement.
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

  # The package of a plan by name, as the plan records it.
  packageOf =
    plan: name:
    lib.findFirst (p: p.name == name) (throw "units: ${name} is not a package of the plan ${plan.name}")
      (lib.attrValues plan.graph.packages);

  # The units of one package in a plan: its library, binaries and test
  # executables, each keyed by the package, its version and the unit.
  unitsOf =
    plan: name:
    let
      package = packageOf plan name;
      prefix = "${package.name}-${package.version}-";
    in
    lib.attrValues (lib.filterAttrs (key: _: lib.hasPrefix prefix key) plan.graph.units);

  # The fee switch on the one crate that reads it. ubos-config takes
  # `DEVFEE` through `option_env!`, so the value has to reach that
  # crate's compile, which an override's environment does. A plan
  # without the crate leaves the entry unused.
  feeOverride = fee: {
    ubos-config = {
      env = fee;
      optional = true;
    };
  };

  # The test runs of one package in a plan.
  testsOf =
    plan: name: lib.attrValues (lib.filterAttrs (_: test: test.packageName == name) plan.tests);
in
{
  # A plan over one host crate, as `cargo build -p` and `cargo test -p`
  # plan it: its units, and with `tests` its test executables too,
  # built and run, in the dev profile so debug_assert! and overflow
  # checks fire. One crate per plan, because cargo unifies the features
  # of what it plans together, and the host crates disagree on them.
  # `fee` is the environment of the fee switch, from nix/fee.nix.
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

  # One crate's release build for a board target: an image at
  # `bin/<name>` of the result when the crate is a board. `env` is the
  # crate's own build environment; `workspaceEnv` is what cargo's
  # `[env]` gives every unit, which a board's own `.cargo/config.toml`
  # states but cargo reads only from the workspace root it plans from,
  # so the plan's source carries it there. `fee` is the environment of
  # the fee switch, from nix/fee.nix.
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
