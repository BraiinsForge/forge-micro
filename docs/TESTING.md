# Testing

The firmware workspace mixes pure `no_std` libraries, firmware binaries, host
runners, frontend assets, and hardware behavior. A single workspace coverage
number is not a useful signal: `cargo test --workspace` can unify incompatible
embedded features, while firmware runtime behavior needs board builds and
hardware smoke tests rather than host line coverage.

Use layered checks instead. Each layer should have a clear owner, runtime, and
failure meaning.

## Test quality

A test should exercise behavior, not restate the source. Do not write tests
that only confirm:

- a constructor stored the arguments it was handed;
- a `new` or `default` value has its fields zeroed or set to the obvious
  literal;
- a `const` equals its own defining expression or literal;
- an enum variant carries the payload it was constructed with.

These pass by construction and break only when the source is edited to match,
so they catch nothing. Delete them. A boundary that a constructor enforces (a
length limit, a range check) is behavior and stays; test the accept and reject
edges, not the trivial store.

When a test asserts the contents of a struct, compare the whole value against
one expected literal in a single `assert_eq!`, not field by field. Field-by-
field assertions silently skip fields and drift as the struct grows, and a
whole-value comparison reads as the one fact the test is pinning. Derive
`PartialEq` and `Debug` on the struct when a test needs them; that is a fair
cost for an honest assertion.

The exception is a packed register or bitfield type with no public named
fields to write in a literal. There, assert the packed value against the known
constant and let the decoded-field assertions document the bit layout, which is
the unit actually under test.

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

This is the default pull-request gate. It should stay quick and deterministic,
with no hardware, web bundler, network service, or probe dependency.

Run the layer through flake checks from the repository root:

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
- `stratum-v2` has state-ring, frame, and error-path tests matching the
  `stratum-v1` standard.
- `stratum-v2` has golden-transcript replay tests for a successful
  handshake/channel-open and a rejection path, anchoring the encrypted wire
  format against committed byte fixtures.

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

Run each board in the correct Nix devshell. The per-board matrix lives
in [BOARDS.md](BOARDS.md).

For each board, check the default feature set. The gate should catch
target dependency drift, linker-script failures, accidental feature coupling,
and oversized binaries before hardware testing.

The flake exposes this as `build-<board>` for the default feature set.

Run this layer through the aggregate flake check:

```sh
nix build .#checks.x86_64-linux.firmware-builds --no-link
```

## Web and host-runner tests

`ubos-webui` requires `nodejs` in `PATH`, or the hermetic
`UBOS_WEBUI_DIST` env the flake checks set; run it only in a devshell
that provides one of the two. Keep this separate from the fast host
Rust gate so a missing frontend tool does not hide library test
failures.

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

### HTTP listener load test

A pool-depth change must be verified against a real board before
merging: drive `N` jittered pollers at the listener, sweep the
concurrency, and record success rate and latency per level. That sweep
is also what populates the capacity table in `docs/HTTP_LISTENERS.md`,
which documents the method and the parameters the published numbers
were taken at.

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

## Documentation coverage

Two checks find the functions that a documentation audit missed. The
compiler answers both, because a script that reads the source can miss a
declaration.

**Which free functions does a crate declare?** `rustdoc` lists every
item that a crate declares. With private items included, the `all.html`
page names the free functions and not the methods:

```sh
cargo doc -p <crate> --document-private-items --no-deps
```

Three reasons put a name on that page against no source line, and none
of them is a miss: a macro emits the function, the crate re-exports it
from another crate, or a `#[cfg]` hides the declaration from the run.

**Which items have no doc comment?** Two lints answer this. Add both to
the crate root as a temporary edit, then run clippy for that crate:

```rust
#![warn(missing_docs)]
#![warn(clippy::missing_docs_in_private_items)]
```

Both lints are needed. `rustc` reads `missing_docs` for public items
alone, and the clippy lint covers the private items. Neither one reads a
`//` comment as documentation. The two lints also warn on modules,
constants, statics, fields and variants, so read past the other kinds.

CAUTION: Remove the two attributes before you commit. A permanent gate
for them is a separate decision. It costs far more than the functions
did, because both trees carry hundreds of fields, constants and variants
with no doc comment.

Run each crate on its own, from the crate directory, in the shell for
its target. Do not pass several crates to one `cargo doc`. That unions
their features, and `critical-section` then fails to build. One run
covers one compiled configuration. A `#[cfg]` that is off is invisible
to it, so a crate with features, or with more than one target, needs
one run for each configuration that ships. `ubos-ota`, `ubos-core`,
`ubos-thermal` and `ubos-config` are the crates where that limit bites.

## Roadmap

Completed:

- Fast host crates are covered by isolated `test-*` flake checks and the
  `fast-host` aggregate.
- Firmware default-feature release builds are covered by per-board
  checks and the `firmware-builds` aggregate.
- Web UI and native host-runner tests are covered by the `web-host` aggregate
  with the hermetic webui dist supplied by the flake.
- `cargo-llvm-cov` is available in the host devshell for per-crate coverage
  reporting.
- `stratum-v2` has pure `V2SessionState` tests for defaults, job-id ring
  rotation, unknown job-id lookup, sequence wrapping, and pending future jobs.
- `stratum-v2` has frame and error-path integration tests covering future-job
  activation, partial reads/writes, mid-frame cancellation and resume, decode
  failure, rejected sessions, and submit-before-channel-open.
- `stratum-v2` has known-answer golden-transcript replay tests: a seeded RNG
  pins all handshake randomness so the encrypted wire bytes are reproducible,
  committed as fixtures, and replayed through the real client transport for
  both a success and a rejection path.

Remaining:

- Define hardware-in-loop smoke checks once board availability, probe mapping,
  expected network identities, and result collection are stable.
