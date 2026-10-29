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

# The layer table of the ubos crates and the gate that holds the
# dependency graph to it.
#
# A crate may depend only on crates in its own layer or a lower one, so
# the dependency direction docs/STRUCTURE.md describes is checked, not
# trusted. The hardware crates, the HALs and the runtimes, are named only
# from the platform layer up: a crate below it builds on every target and
# can be tested on the host. Every workspace member must sit in exactly
# one layer, so a new crate cannot join the graph without a place in it.
#
# The gate reads the normal and target-specific dependencies of each
# member's manifest during evaluation, by package name. Dev-dependencies
# are left out, because a test may reach anything it exercises, and so
# are build-dependencies, which run on the build machine. A dependency
# is named by its `package` key in the member's own manifest, so a
# rename inherited from `[workspace.dependencies]` is not seen; no
# workspace dependency renames a package today.
{ pkgs }:
let
  inherit (pkgs) lib;

  # The layers from the lowest up. A name in no layer is outside the
  # table, which is how an external crate is read.
  table = [
    {
      name = "foundation";
      crates = [
        "ubos-core"
        "transitions"
        "ubos-logging"
        "ubos-metrics"
        "ubos-led"
        "serial-baudrate"
        "mining-protocol"
        "ii-sv2"
        "ii-sv2-types"
        "ii-sv2-framing"
        "ii-sv2-messages"
        "ii-sv2-security"
        "ii-sv2-crypto"
      ];
    }
    {
      name = "drivers";
      crates = [
        "ubos-asic"
        "ubos-config"
        "ubos-eeprom"
        "ubos-temperature"
        "ubos-auth"
        "ubos-events"
        "ubos-http"
        "ubos-mdns"
        "stratum-v1"
        "stratum-v2"
      ];
    }
    {
      name = "services";
      crates = [
        "ubos-net"
        "ubos-prometheus"
        "ubos-webui"
        "stratum-client"
        "ubos-psu"
        "ubos-tuner"
        "ubos-machine"
        "ubos-mining"
        "ubos-manager"
        "ubos-thermal"
        "ubos-cooling"
        "ubos-display"
        "ubos-api"
        "ubos-ota"
      ];
    }
    {
      name = "platform";
      crates = [
        "ubos-rp"
        "ubos-esp32"
        "rp-bootloader"
        "esp32s3-tsens"
        "rp235x-sha256"
      ];
    }
    {
      name = "assembly";
      crates = [ "ubos-firmware" ];
    }
    {
      name = "boards";
      crates = [
        "bmm-pico"
        "esp32s3"
        "forge-axe"
        "hashnode"
        "rp2040"
        "rp2040-bootloader"
        "rp2350"
        "rp2350-bootloader"
      ];
    }
    {
      name = "host";
      crates = [
        "embassy-net-afpacket"
        "embedded-i2c-unix"
        "embedded-serial-unix"
        "host-runner"
        "ubos-build"
        "ubos-main-host"
        "ubos-snapshot"
      ];
    }
  ];

  # The crates that tie a crate to a chip or a core: the HALs, their PACs,
  # the runtimes and the boot and flash crates built on them.
  hardware = [
    "cortex-m"
    "cortex-m-rt"
    "embassy-boot"
    "embassy-boot-rp"
    "embassy-rp"
    "esp-bootloader-esp-idf"
    "esp-hal"
    "esp-rtos"
    "esp-storage"
    "riscv"
    "riscv-rt"
    "rp-pac"
    "xtensa-lx"
    "xtensa-lx-rt"
  ];

  # The lowest layer that may name a hardware crate.
  hardwareFloor = "platform";

  # `table` with `additions`, a layer name to a list of crates, appended
  # to the named layers. The internal repository adds its crates this way.
  extend =
    additions:
    map (layer: layer // { crates = layer.crates ++ (additions.${layer.name} or [ ]); }) table;

  # The gate over the workspace at `root`. `extraHardware` names the
  # hardware crates of a chip this repository does not support, which a
  # consumer supplies.
  check =
    {
      root,
      layers ? table,
      extraHardware ? [ ],
    }:
    let
      readManifest = dir: builtins.fromTOML (builtins.readFile (root + "/${dir}/Cargo.toml"));
      members = map readManifest (readManifest ".").workspace.members;

      indexed = lib.imap0 (index: layer: { inherit index layer; }) layers;
      layerOf = builtins.listToAttrs (
        builtins.concatMap (
          entry:
          map (crate: {
            name = crate;
            value = entry;
          }) entry.layer.crates
        ) indexed
      );
      floor = (lib.findFirst (entry: entry.layer.name == hardwareFloor) null indexed).index;

      # The package names a manifest depends on outside its tests and its
      # build script.
      depsOf =
        manifest:
        let
          tables = [
            (manifest.dependencies or { })
          ]
          ++ map (target: target.dependencies or { }) (builtins.attrValues (manifest.target or { }));
        in
        lib.unique (builtins.concatMap (lib.mapAttrsToList (name: spec: spec.package or name)) tables);

      violationsOf =
        manifest:
        let
          name = manifest.package.name;
          own = layerOf.${name};
          upward = dep: layerOf ? ${dep} && layerOf.${dep}.index > own.index;
          tooLow = dep: builtins.elem dep (hardware ++ extraHardware) && own.index < floor;
          judge =
            dep:
            if upward dep then
              [ "${name} (${own.layer.name}) depends on ${dep} (${layerOf.${dep}.layer.name})" ]
            else if tooLow dep then
              [ "${name} (${own.layer.name}) depends on the hardware crate ${dep}" ]
            else
              [ ];
        in
        if layerOf ? ${name} then
          builtins.concatMap judge (depsOf manifest)
        else
          [ "${name} is in no layer of nix/layers.nix" ];

      violations = builtins.concatMap violationsOf members;
    in
    pkgs.runCommand "layers" { } (
      if violations == [ ] then
        "touch $out"
      else
        ''
          echo "The crate graph breaks the layer table in nix/layers.nix:" >&2
          ${lib.concatMapStrings (v: "echo ${lib.escapeShellArg "  ${v}"} >&2\n") violations}
          echo "" >&2
          echo "A crate depends only on its own layer or a lower one, and only" >&2
          echo "the ${hardwareFloor} layer and above name a hardware crate." >&2
          exit 1
        ''
    );
in
{
  inherit table extend check;
}
