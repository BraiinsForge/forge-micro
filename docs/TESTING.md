# Testing

`ubos-main` mixes pure `no_std` libraries, firmware binaries, host runners,
frontend assets, and hardware behavior. A single workspace coverage number is
not a useful signal: `cargo test --workspace` can unify incompatible embedded
features, while firmware runtime behavior needs board builds and hardware smoke
tests rather than host line coverage.

Use layered checks instead. Each layer should have a clear owner, runtime, and
failure meaning.

## Fast host tests

Run host-testable crates package by package. This avoids Cargo feature
unification across unrelated firmware targets and avoids duplicate Embassy
time-driver symbols between crates that provide host-test stubs.

Primary crates:

- `embedded-i2c-unix`
- `mining-protocol`
- `stratum-client`
- `stratum-v1`
- `ubos-api`
- `ubos-auth`
- `ubos-config`
- `ubos-cooling`
- `ubos-http`
- `ubos-manager`
- `ubos-metrics`
- `ubos-mining`
- `ubos-net`
- `ubos-psu`
- `ubos-temperature`
- `ubos-thermal`

Standalone host utilities:

- `nuc/pkgs/load-test`

This is the default pull-request gate. It should stay quick and deterministic,
with no hardware, web bundler, network service, or probe dependency.

Run the layer through flake checks from `boards/ubos-main`:

```sh
nix flake check
```

The fast host layer is represented by individual `test-*` flake checks rather
than a shell script. CI can build the whole flake or select named checks such as
`checks.x86_64-linux.test-ubos-config`. To run just this layer, build the
aggregate check:

```sh
nix build .#checks.x86_64-linux.fast-host --no-link
```

## Protocol contract tests

Protocol crates should have the strongest host coverage because they are pure
logic and regressions can break all boards.

- `stratum-v1` already has parser, wire, state, target, and async client tests.
- `mining-protocol` covers hashing, difficulty, hex parsing, and merkle logic.
- `stratum-v2` has state-ring tests; it still needs frame and error-path tests
  before it matches the `stratum-v1` standard.

Remaining `stratum-v2` coverage:

- Future-job storage and activation around `NewMiningJob` /
  `SetNewPrevHash`.
- Fake async-stream tests for partial reads, partial writes, oversized frames,
  decode failure, rejected sessions, and submit-before-channel-open.
- Golden transcript replay for at least one successful pool handshake/channel
  open and one rejection path.

## Driver and control tests

Board-adjacent behavior should be pushed into host-testable libraries or small
pure helpers. Firmware `main.rs` files should remain wiring code.

Cover with mocks or pure tests:

- PSU state transitions, register encoding, miniminer watchdog behavior.
- Fan PID behavior and tach/RPM conversion.
- Thermal thresholds and pause/resume reasons.
- BM1362 frame parsing, reset sequencing, and serial actor error handling.
- Config validation, persistence format, and migration/default behavior.
- Metrics formatting and hashrate smoothing.

The rule is: if a behavior can be expressed without owning a hardware
peripheral singleton, it belongs in a host-testable crate or helper.

## Firmware build matrix

Firmware crates need compile coverage, feature coverage, and binary-size
visibility. They do not need host unit tests for every HAL call.

Run each board in the correct Nix devshell:

- `firmware/rp2040`
- `firmware/rp2350`
- `firmware/bmm-pico`
- `firmware/stm32f407`
- `firmware/stm32f439`
- `firmware/esp32s3`

For each board, check default features and important variants such as
`--no-default-features` where `display` is optional. The gate should catch
target dependency drift, linker-script failures, accidental feature coupling,
and oversized binaries before hardware testing.

The flake exposes this as `build-<board>` for the default feature set and
`build-<board>-no-display` for the `--no-default-features` variant. The ESP32-S3
entries use the same naming pattern with the Xtensa toolchain derivation.

Run this layer through the aggregate flake check:

```sh
nix build .#checks.x86_64-linux.firmware-builds --no-link
```

## Web and host-runner tests

`ubos-webui` requires `esbuild` in `PATH`; run it only in a devshell that
provides the bundler. Keep this separate from the fast host Rust gate so a
missing frontend tool does not hide library test failures.

`host-runner` should have two tracks:

- unit tests for logic that does not require `ubos-webui` or host devices;
- a devshell integration build that includes the full web UI and host network
  stack.

If necessary, split host-runner logic into a small library crate so its unit
tests do not need to build the executable dependency graph.

Run this layer through the aggregate flake check:

```sh
nix build .#checks.x86_64-linux.web-host --no-link
```

## Hardware-in-loop smoke tests

Hardware tests should be small, explicit, and slower than the PR gate. Run them
nightly, before releases, or before merging firmware changes that affect board
bring-up.

Recommended smoke checks:

- flash firmware to a selected board;
- wait for DHCP and mDNS;
- request `/api/info`;
- request Prometheus metrics and verify required metric names;
- wait for NTP time to become valid;
- verify mining state reaches expected idle or active state;
- observe for a fixed window with no watchdog reset.

Each supported MCU family should have at least one board in the smoke matrix.

## Coverage reporting

Use `cargo-llvm-cov` from the Nix devshell for host-testable crates only.
Report per-crate coverage instead of one global threshold.

Run coverage from the host devshell and keep the same per-package isolation as
the fast host layer:

```sh
nix develop .#host -c cargo llvm-cov -p ubos-config --html
```

Suggested policy:

- high threshold for protocol, config, auth, API, control, and parser crates;
- moderate threshold for hardware-driver crates with mockable register logic;
- no line threshold for firmware binaries, frontend generated assets, or
  vendored patches;
- build/HIL gates cover firmware glue that cannot be meaningfully host-tested.

Coverage should fail only when it points to missing meaningful behavior tests,
not because generated or board-only wiring code drags down a global percentage.

## Roadmap

Completed:

- Fast host crates are covered by isolated `test-*` flake checks and the
  `fast-host` aggregate.
- Firmware default and no-display release builds are covered by per-board
  checks and the `firmware-builds` aggregate.
- Web UI and native host-runner tests are covered by the `web-host` aggregate
  with `esbuild` supplied by the flake.
- `cargo-llvm-cov` is available in the host devshell for per-crate coverage
  reporting.
- `stratum-v2` has pure `V2SessionState` tests for defaults, job-id ring
  rotation, unknown job-id lookup, sequence wrapping, and pending future jobs.

Deferred:

- Add `stratum-v2` frame and error-path tests when protocol coverage becomes
  the next focus area.

Remaining:

- Define hardware-in-loop smoke checks once board availability, probe mapping,
  expected network identities, and result collection are stable.
