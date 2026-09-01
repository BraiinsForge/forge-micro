# The Architecture Gate

How to judge a branch against the architecture documented in `docs/`.
Read this before running the architecture phase. The docs are the
authority; this file only routes to them, separates what is normative
from what is historical, and names the violation classes worth hunting.
It never restates an invariant, so when the docs move, this file does
not have to.

When the review runs in the private consumer repo, the same docs govern
its crates too — they live under `ubos-main/docs/` there, and every path
below reads through that prefix.

## Contents

- When the phase applies
- What to read, routed by the diff
- Normative and historical
- What to hunt
- Doc drift is the other half
- Reporting

## When the phase applies

The phase judges code against the architecture, so it applies when the
diff touches a crate: anything under `ubos/`, `firmware/`, or the
consumer repo's own crates. A diff of only docs, harness config, or CI
wiring gives it nothing to judge; record
`architecture gate: SKIPPED (no crate changed)` and move on. A docs-only
diff that changes `docs/` itself is still checked — but by the drift
direction below, code against doc, not the reverse.

## What to read, routed by the diff

Read only what the touched paths route to. The point of routing is that
the phase costs one or two documents, not the directory.

Always, when any `ubos-*` crate or firmware crate changed:

- `docs/ARCHITECTURE_PROPOSAL.md` §2 (design principles), §3 (the layer
  map), and §5 (the communication contract). These three sections are
  the cross-cutting law; everything else is per-subsystem.

Then per touched subsystem:

| Diff touches | Read |
|---|---|
| `ubos/ubos-asic`, the mining serial actor or tuner in `ubos/ubos-mining` | `docs/ASIC.md`; `docs/REGISTERS.md` when register layouts change |
| `ubos/stratum-*`, `ubos/mining-protocol`, work dispatch | `docs/STRATUM_ARCHITECTURE.md`, `docs/MINING.md` |
| a `firmware/*` crate's wiring, pins, or size budget | `docs/BOARDS.md`; `docs/OVERVIEW.md` for hashnode |
| OTA paths | `docs/OTA.md` |
| `ubos/ubos-api`, HTTP surfaces | `docs/REST_API.md`, `docs/HTTP_LISTENERS.md` |
| `ubos/ubos-net`, address handling | `docs/NET_ADDRESS_OWNERSHIP.md` |
| metrics, events, machine model, EEPROM, multipool | the same-named doc under `docs/` |

The general rule behind the table: `docs/` is indexed by subsystem, so a
diff touching subsystem X reads the doc named for X when one exists.
`ARCHITECTURE_PROPOSAL.md` §4 carries the per-module API shapes for the
subsystems it covers; read the relevant module's entry when the diff
changes that module's public surface.

## Normative and historical

`ARCHITECTURE_PROPOSAL.md` interleaves law with record. Judge against
the law; never raise a finding from the record:

- **Normative**: §2 (principles), §3 (layer map), §4 (module APIs),
  §5 (communication contract and its rules), Appendix B (what each
  crate is for).
- **Historical**: §1 (the state the proposal started from) and
  Appendix A (the migration steps, kept as a record of how the code got
  here). A mismatch between Appendix A's intermediate states and the
  final code is documented there and is not a finding.

The other docs under `docs/` are reference documents and normative
throughout.

## What to hunt

Each class below is a §-reference into `ARCHITECTURE_PROPOSAL.md`
unless another doc is named. Verify against the doc, not against this
list's wording.

- **A dependency edge against the layer map.** A new `Cargo.toml`
  dependency or `use` that points upward or sideways across §3 — for
  example `ubos-config` importing the manager or mining, or `ubos-asic`
  importing `ubos-mining` (the arrow is `ubos-mining → ubos-asic`,
  `ASIC.md`).
- **Cross-module traffic outside the contract.** A new channel, watch,
  or signal between modules that §5's table does not carry, or a sender
  reaching a module's private statics instead of its `…Handle`. The
  table's own rule applies: if it is not on the list, the modules do
  not talk.
- **A `pub static` crossing a crate boundary.** §2 says handles, not
  statics; the metrics push channel is the one sanctioned exception.
- **A second writer.** Two crates writing one piece of state, or any
  caller of `mining.halt`/`resume` other than the manager's `run`.
- **A module off the template.** A new or reshaped subsystem that skips
  the §2 principle-11 skeleton: the `Copy` handle over private statics,
  one `run` task draining one command channel, no public trait.
- **Scaffolding on the wire.** Public `Assert(Reason)`/`Clear(Reason)`
  style commands instead of domain messages (§2 principle 13).
- **I/O or policy on the wrong side of the sans-IO seam.** UART or
  other I/O inside `ubos-asic`, or tuning policy (voltage, ramp, work
  start) inside the vendor driver instead of the tuner (`ASIC.md`).
- **Bare numbers at module APIs** where `ubos-core`'s typed units exist
  (§2 principle 9).

A diff may violate the docs deliberately — the architecture does move.
That is still an `ARCH` finding unless the same branch updates the doc;
when it does, verify code and doc agree and raise nothing.

## Doc drift is the other half

Compliance runs both ways. When the diff changes behaviour that a
routed doc describes — a bring-up step, a wire-table row, a board's
pinout or size budget, a module's public surface — and the branch does
not touch the describing doc, that is an `ARCH` finding too: the code
now disagrees with its documentation. Name the doc section that went
stale.

## Reporting

`ARCH` findings carry a file:line into the diff plus the doc and
section they violate:

```
ARCH ubos/ubos-cooling/src/pid.rs:88 -- cooling writes mining.halt directly; only the manager writes halt/resume (ARCHITECTURE_PROPOSAL.md §5)
ARCH docs/ASIC.md:210 -- bring-up gained a step the doc's sequence does not carry (doc drift)
```

Cite the section, not a line number into the doc — the docs move too
often for line references to hold.
