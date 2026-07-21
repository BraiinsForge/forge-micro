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

# probe-rs fork tooling: the redacted-capable fork build, the remote and
# server variants with their patches, and the overlay exposing them
# plus the probe-rs-runner wrapper. See flake.nix for how the overlay
# is threaded into each system's pkgs and the NUC's cross overlay.
{ inputs }:
rec {
  # probe-rs fork carrying redacted redacted, RISC-V debug spec 0.11, support:
  # upstream 0.31.0 plus one commit adding the redacted target. The dependency
  # closure still matches nixpkgs' own 0.31.0 probe-rs-tools, so the whole
  # nixpkgs build recipe is reused and only the source and the re-vendored
  # cargo deps are swapped. Every probe-rs consumer, each dev shell and the
  # NUC serve service, builds from this one base, so redacted flashing and RTT
  # work across the fleet without a second probe-rs version. cargoHash is
  # baked into nixpkgs' derivation before overrideAttrs runs, so the vendor
  # must be rethreaded through cargoDeps rather than by setting cargoHash.
  # The fork source arrives through the probe-rs-redacted flake input, fetched
  # at eval time so the gitlab.ii.zone SSH access uses the developer's git
  # config; the sandbox carries no ssh.
  #
  # cargoDeps uses importCargoLock rather than fetchCargoVendor: this vendor
  # is a binary-cache miss, and fetchCargoVendor's parallel download pool
  # trips crates.io's rate limit on the /download API endpoint, HTTP 403,
  # while fetching the full closure fresh. importCargoLock fetches each
  # crate as its own cache-backed fetchurl, so most come from cache.nixos.org
  # and the rest go through nix's throttled, retrying fetcher. The fork's
  # lock has no git sources, so no outputHashes are required.
  mkForkProbeRsTools =
    pkgs:
    let
      forkSrc = inputs.probe-rs-redacted;
    in
    pkgs.probe-rs-tools.overrideAttrs (old: {
      src = forkSrc;
      cargoDeps = pkgs.rustPlatform.importCargoLock {
        lockFile = "${forkSrc}/Cargo.lock";
      };
      # The fork renames the DAP test `test_initalize_request`, a nixpkgs
      # typo, to `test_initialize_request`, so nixpkgs' existing --skip no
      # longer matches and the probe-requiring test runs and fails. Skip it
      # under its corrected name.
      checkFlags = (old.checkFlags or [ ]) ++ [
        "--skip=cmd::dap_server::server::debugger::test::test_initialize_request"
      ];
    });

  # `serve` panics on `ConnectionClosed` when the client disconnects
  # mid-RPC, leaving the probe Session held but the worker dead.
  # systemd does not restart because the panic is tokio-contained,
  # so the next client gets a silent exit-0. No upstream fix yet.
  mkRemoteProbeRsTools =
    base:
    base.overrideAttrs (old: {
      cargoBuildFeatures = (old.cargoBuildFeatures or [ ]) ++ [ "remote" ];
      patches = (old.patches or [ ]) ++ [
        ../patches/probe-rs-rpc-tolerate-disconnect.patch
      ];
    });

  # The bench's probe-rs server additionally needs the serve-side TCP
  # keepalive fix so a client that dies abruptly does not pin the probe
  # Session. That patch is server-only, so it stays off the dev-facing
  # probe-rs-tools-remote, which gains nothing from it.
  mkServerProbeRsTools =
    pkgs: base:
    let
      serverPatch = ../patches/probe-rs-serve-tcp-keepalive.patch;
    in
    (mkRemoteProbeRsTools base).overrideAttrs (old: {
      patches = (old.patches or [ ]) ++ [ serverPatch ];
      # The server patch adds socket2 to probe-rs-tools' Cargo.toml and
      # Cargo.lock, so the vendor must list it under probe-rs-tools too.
      # Re-vendor from the patched source's lock rather than reusing the
      # remote vendor: importCargoLock produces a self-referential vendor
      # directory that cannot be copied to a new path, so the old in-place
      # lock swap no longer applies. socket2 is already in the closure via
      # tokio, so this re-vendor is a set of cache hits. Reading the lock
      # from the applyPatches output is import-from-derivation and relies on
      # IFD being enabled, the default here, unlike the fork base above
      # whose lock comes from the already-fetched flake input.
      cargoDeps =
        let
          patchedSrc = pkgs.applyPatches {
            # The fork src is a flake-input store path with no name to
            # derive a default from, so name the patched source explicitly.
            name = "probe-rs-server-src";
            inherit (old) src;
            patches = (old.patches or [ ]) ++ [ serverPatch ];
          };
        in
        pkgs.rustPlatform.importCargoLock {
          lockFile = "${patchedSrc}/Cargo.lock";
        };
    });

  overlay = final: prev: {
    probe-rs-tools-remote = mkRemoteProbeRsTools (mkForkProbeRsTools prev);
    probe-rs-tools-server = mkServerProbeRsTools prev (mkForkProbeRsTools prev);
    probe-rs-runner = prev.writeShellScriptBin "probe-rs-runner" ''
      args=()
      for arg in "$@"; do
        if [ "$arg" = "--local" ]; then
          unset PROBE_RS_REMOTE_HOST PROBE_RS_REMOTE_TOKEN
        else
          args+=("$arg")
        fi
      done
      # Default RUST_LOG=info so probe-rs surfaces session errors
      # to stderr instead of returning silently with exit 0 when a
      # CMSIS-DAP probe is wedged (probe-rs/probe-rs#1849).
      # Caller-set RUST_LOG wins.
      export RUST_LOG="''${RUST_LOG:-info}"
      # Keep probe-rs's ANSI colors alive through the tee. A bare
      # pipe makes probe-rs see a non-tty and drop color on both the
      # terminal and the saved log. probe-rs builds on anstream,
      # which honors CLICOLOR_FORCE; NO_COLOR still wins and a
      # caller-set value is preserved. The saved log keeps the
      # escapes; read it back with `less -R`.
      if [ -z "''${NO_COLOR:-}" ]; then
        export CLICOLOR_FORCE="''${CLICOLOR_FORCE:-1}"
      fi
      # Persist a copy of each session so a flash can be left
      # unattended and read back later. Logs land in the project's
      # logs/ directory - the nearest ancestor holding flake.nix,
      # i.e. ubos-main. PROBE_RS_LOG_DIR overrides this, and the NUC
      # layers rotation on top of that override separately.
      #
      # Named <firmware>[-<suffix>]-<timestamp>.log. The firmware
      # name is the crate directory we run from (rp2350, bmm-pico,
      # ...). The suffix is the probe serial of the flashed board,
      # taken from $PROBE_SERIAL, which the cargo run-N aliases set
      # to distinguish the six bmm-pico units; it is appended when
      # present. The timestamp keeps one file per flash rather than
      # overwriting.
      fw="$(basename "$PWD")"
      serial="''${PROBE_SERIAL:-}"
      suffix="''${serial##*:}"
      stamp="$(date +%Y%m%dT%H%M%S)"
      log_dir="''${PROBE_RS_LOG_DIR:-}"
      if [ -z "$log_dir" ]; then
        root="$PWD"
        while [ "$root" != "/" ] && [ ! -e "$root/flake.nix" ]; do
          root="$(dirname "$root")"
        done
        if [ -e "$root/flake.nix" ]; then
          log_dir="$root/logs"
        else
          log_dir="$PWD/logs"
        fi
      fi
      log_file="$log_dir/$fw''${suffix:+-$suffix}-$stamp.log"
      mkdir -p "$log_dir"
      echo "probe-rs-runner: logging to $log_file" >&2
      start=$(date +%s)
      # Run probe-rs as a tracked child rather than the tail of a
      # `| tee` pipeline. In a pipeline probe-rs lives in a subshell,
      # so the documented scripted detach `kill -INT <runner-pid>`
      # reaches this shell but never probe-rs. Backgrounding it
      # behind a tee process substitution keeps probe-rs's pid in
      # hand; the INT trap forwards the signal so detach reaches the
      # client (see CLAUDE.md).
      probe-rs "''${args[@]}" > >(tee "$log_file") 2>&1 &
      probe_pid=$!
      trap 'kill -INT "$probe_pid" 2>/dev/null' INT
      while true; do
        wait "$probe_pid"
        status=$?
        # A status >128 means a trapped signal interrupted the wait.
        # If probe-rs is still alive it is mid-detach, so keep
        # waiting; otherwise that status is probe-rs's own exit.
        if [ "$status" -gt 128 ] && kill -0 "$probe_pid" 2>/dev/null; then
          continue
        fi
        break
      done
      trap - INT
      elapsed=$(( $(date +%s) - start ))
      # A real flash plus attach of any of our targets takes longer
      # than this threshold. An exit-0 in under that with `run`
      # requested means probe-rs short-circuited without ever
      # talking to the chip - canonically the wedged CMSIS-DAP case
      # where the remote server swallows the error. Surface a hint
      # so the failure isn't silent.
      min_flash_seconds=4
      if [ "$status" -eq 0 ] && [ "$elapsed" -lt "$min_flash_seconds" ]; then
        for arg in "''${args[@]}"; do
          if [ "$arg" = "run" ] || [ "$arg" = "attach" ]; then
            echo "probe-rs-runner: probe-rs exited 0 in ''${elapsed}s without flashing - probe likely wedged (probe-rs/probe-rs#1849 for CMSIS-DAP)." >&2
            echo "  Physically replug the probe; check 'journalctl -u probe-rs-server' on the NUC for server-side errors." >&2
            break
          fi
        done
      fi
      if [ "$status" -ne 0 ]; then
        echo "probe-rs-runner: probe-rs exited $status" >&2
      fi
      exit $status
    '';
  };
}
