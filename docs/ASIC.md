# ASIC Mining Driver

Reference for the BM13xx hashboard driver: the chip support split across
`ubos-asic` (pure, sans-IO chip logic) and `ubos-mining` (the serial
I/O edge plus tuning policy). Supported chips: **BM1362, BM1366,
BM1370**. Register bit layouts are documented separately in
[`REGISTERS.md`](REGISTERS.md); this document covers the architecture,
the bring-up sequence, and the per-chip differences.

## Crate responsibilities

- **`ubos-asic`** — pure and unit-testable, owns no UART and performs no
  direct I/O. Holds the typed register layouts, the wire framing
  (`HwWork` / `HwSolution` / `ResponseParser`), the `ChipDescriptor` /
  `HashboardDescriptor` data, the `ChipProtocol` / `ChipBus` traits, and the
  per-chip bring-up recipes (`Bm1362` / `Bm1366` / `Bm1370`).
- **`ubos-mining`** — owns the serial actor (`SERIAL`) that drives the
  UART halves, the `ChipBus` implementation that forwards to the actor,
  the tuner that applies bring-up and the V²·f ramp, and the work
  dispatch / share verification path.

The dependency arrow is `ubos-mining → ubos-asic`. `ubos-asic` declares
`ChipBus`; `ubos-mining` implements it. This is the sans-IO rule:
bring-up logic computes and acts through a passed-in bus, so it is
testable against a mock bus, and the actor stays the single I/O edge.

## Hardware model

A hashboard is a chain of chips in series. Power is organized into
**voltage domains**: a domain is one series element on the rail, and a
domain may contain more than one chip wired in parallel.

`HashboardDescriptor` carries the topology:

| Field | Meaning |
|-------|---------|
| `chip` | the `Chip` model populated on the board |
| `num_chips` | total chips on the serial chain |
| `num_voltage_domains` | series voltage domains on the rail |
| `max_psu_voltage` | upper rail bound the PSU enforces |

`chips_per_domain = num_chips / num_voltage_domains`. Chips within a
domain are in parallel; domains are stacked in series, so the open-core
rail voltage is `chip.max_chip_voltage * num_voltage_domains` clamped to
`max_psu_voltage`.

The boards defined today are `BMM101_HASHBOARD` (four BM1362 chips) and
`BFMX4_HASHBOARD` (four BM1370 chips), each with four domains, one chip
per domain.

Chip-internal voltage stacking inside a package is **not modeled** — it
is a chip implementation detail with no bearing on the driver. All
domain handling is driven by the board topology above, uniformly across
every chip model.

### Per-chip facts

| Chip | `chip_id` | cores | max chip mV | frequency range | work-id variants |
|------|-----------|-------|-------------|-----------------|------------------|
| BM1362 | `0x1362` | 514 | 422 | 50–1000 MHz | 8 |
| BM1366 | `0x1366` | 894 | 1382 | 50–1000 MHz | 8 |
| BM1370 | `0x1370` | 2040 | 1250 | 250–1000 MHz | 16 |

The frequency-range upper bound is a uniform 1000 MHz local
tuning-policy envelope for all three chips, not a chip-spec maximum; the
chip has no hard limit there. All three share the 6.25 MHz frequency
step. The `max chip mV` values are per-chip safety/tuning policy, not
chip-spec maxima.

The **work-id variant count** is explained under [Work dispatch](#work-dispatch-and-version-rolling).

## Architecture: sans-IO layering

Two traits in `ubos-asic`:

- **`ChipBus`** — the chip-side I/O surface a recipe drives:
  `write_register_raw` / `read_register` / `send_command` / `delay_ms`,
  plus a provided typed `write_register<R>`. The consumer implements it;
  register writes and commands are fire-and-forget, only reads can fail
  (returning an empty responder list).
- **`ChipProtocol`** — the per-chip bring-up recipe as plain imperative
  async steps. The steps are **default methods** keyed on
  `descriptor()`, because the recipe is identical across chip models at
  the level the driver models; a chip overrides only a step where it
  genuinely diverges.

```
ubos-asic  (sans-IO: no UART, no direct I/O)
  registers / framing / descriptors
  ChipBus trait              ← I/O surface; the consumer implements it
  ChipProtocol trait         ← per-chip imperative async bring-up
      Bm1362 / Bm1366 / Bm1370   (recipe via default methods)
      Chip enum                  (dispatches to the above)
ubos-mining
  serial actor (SERIAL)      ← owns the UART halves + select loop
  ChipBus handle             ← thin forwarder over the actor
  tuner (policy)             ← drives ChipProtocol; PSU + V²·f ramp
```

### Chip selection

`ChipProtocol` has async methods and is not `dyn`-compatible, so the
`Chip` enum (`Bm1362` / `Bm1366` / `Bm1370`) implements `ChipProtocol`
and dispatches each call to the concrete recipe. It forwards
`descriptor()` and any overridden step; the shared default-method steps
are inherited.

Selection is **static through the hashboard descriptor**: `HashboardDescriptor.chip`
is a `Chip`, so the tuner drives `board.chip.<step>()`. The chip model
is a compile-time property of the board, not detected at runtime. A
chain is always a single chip model. Each firmware crate's
`config::BOARD` is the single point board selection flows from.

### The serial actor

`SERIAL` owns the UART read and write halves and runs a `select` loop
over three arms: a work tick (periodic work send), a UART read (fed into
`ResponseParser`, dispatched as solutions or register replies), and a
command channel (register reads/writes, baud switch, start/stop work,
reset). On dual-core boards the actor is pinned to Core 1 so the network
stack on Core 0 stays responsive; the shared session uses a
`CriticalSectionRawMutex`.

### Policy stays in the consumer

The recipe does no policy. PSU voltage, the host-side UART baud switch
(run *after* the chip-side `enable_fast_uart`), starting the work loop, and
the V²·f frequency ramp all live in the tuner.

### Testability

Recipes are exercised against a mock `ChipBus` that records every
call (`MockBus` in the `ubos-asic` tests). Tests assert the op sequence —
version rolling precedes the first read, baud is the last register
write, per-domain relay lands on the right chips, the gap count matches
the formula — with no hardware.

## Bring-up sequence

The tuner calls the `ChipProtocol` steps in order. Each step and the
registers it writes (layouts in [`REGISTERS.md`](REGISTERS.md)):

1. **`reset_chain`** — soft-reset to quiescent
   (`SoftResetControlReg`, `0xA8`), misc-control bring-up config
   (`MiscCtrlReg`, `0x18`), then enable hardware version rolling
   (`MidstateCalReg`, `0xA4`) last; it switches responses to the
   11-byte form every later read expects, and is still written before
   the first read in `enumerate_chips`. Finally three broadcast
   `ChainInactive` commands with settle delays so a single lost frame
   is tolerated.
2. **`enumerate_chips`** — assign addresses up the chain (stride 1: a
   chip address equals its chain index), then broadcast-read
   `ChipIdReg` (`0x00`) and count the responders whose `chip_id` matches
   the descriptor.
3. **`initialize_domains`** — broadcast `AnalogMuxControl` disable
   (`0x54`) and the default IO drive strengths (`IoDriverStrengthCtrlReg`,
   `0x58`). When `chips_per_domain > 1`, additionally program, per
   domain, a boosted clock-out drive strength on the domain's last chip
   and the UART relay (`UartRelayReg`, `0x2C`) on its first and last
   chips. Single-chip-per-domain chains (such as `BMM101_HASHBOARD` or
   `BFMX4_HASHBOARD`) need no relay and emit none.
4. **`initialize_cores`** — broadcast core-control configuration
   (`CoreControlReg`, `0x3C`) and the clock-delay value, then per chip a
   core-domain soft reset, misc-control, and the three core-control
   values. Ends by calling `write_misc_settings`.
5. **`write_misc_settings`** — a no-op by default. **BM1370** overrides
   it to write the `MiscSettings` register (`0xB9`, value `0x0000_4480`)
   twice, so a single lost frame still leaves it programmed.
6. **`split_nonce_space`** — program each chip a disjoint nonce offset
   (`ChipNonceOffsetReg`, `0x0C`) by dividing the 16-bit offset space
   into `num_chips` equal slices.
7. **`enable_fast_uart`** — program the chip side of the fast-UART switch
   (`BaudRateReg`, `0x28`). The fast UART is crystal-clocked, so it needs
   no PLL first. The tuner switches the host UART to match afterwards.
8. **`set_hash_freq`** — program the derived hash-counting number
   (`set_hcn`) and the hash-clock PLL (`set_pll`, `PllParameterReg`,
   `0x08`) together, as the frequency ramp pairs them on the fast UART.
   `set_pll` brackets the PLL write with a divider fence and a duplicate
   write to cover a write that arrives garbled because the PLL shifted
   mid-reception. Bring-up does not use the combined form: the fast UART
   is crystal-clocked, so bring-up programs the PLL alone at the
   range-minimum safe-boot baseline before the baud switch and the
   counting number after it, then the ramp walks frequency up from there.

### UART relay gap count

For multi-chip-per-domain chains, the relay inserts inter-byte spacing
that scales with how far a domain sits from the chain head:

```
gap_cnt = chips_per_domain * (num_voltage_domains - domain_index) + 14
```

written into `UartRelayReg` with both relay-enable bits set. The `+ 14`
is a base addend so the relay always inserts a minimum spacing.

## Per-chip differences

At the level the driver models, the bring-up recipe is shared. The
divergences:

- **BM1362** — the baseline. `BMM101_HASHBOARD` is one chip per domain, so
  no UART relay is emitted; the bring-up is the plain shared recipe.
- **BM1366** — identical recipe; a multi-domain board exercises the
  relay / boundary-drive path in `initialize_domains`.
- **BM1370** — adds the `0xB9` `MiscSettings` write after core
  configuration (via the `write_misc_settings` override) and uses 16
  work-id variants instead of 8.

`0xB9` is modeled as an opaque register written as a fixed constant; its
field semantics are uncharacterized and no field names are invented.

## Work dispatch and version rolling

Hardware version rolling is always on: `reset_chain` enables the full
`0xFFFF` mask, the work frame sends a single header (`multiversion = 1`),
and each `HwSolution` carries a `version_idx` that maps to the BIP320
version bits (28:13) for the local hash check.

The chip distributes version-rolling work into a fixed number of
streams and tags each returned solution with a variant index in the low
bits of the echoed `work_id`. That count — the **work-id variant
count** — is 8 for BM1362/BM1366 and 16 for BM1370, carried as
`ChipDescriptor::midstate_count`. The host:

- dispatches work-ids in strides of that count, so the chip's variant
  tags do not collide with the next job's base, and
- recovers the originating job for a returned solution by dividing its
  work-id by that count.

A wrong count misattributes solutions to the wrong job, so it is read
from the descriptor at runtime. The work registry is a fixed array sized
for the smallest count (`256 / 8 = 32` slots); a chip with a larger
count uses fewer slots.

Solutions are verified in place in the actor's `dispatch_response`:
`HwWork::compute_hash` rebuilds the 80-byte header with the rolled
version and double-SHA256s it, and the result is checked against the
target before submission. Nonce-space division across chips is uniform —
the explicit per-chip offset of `split_nonce_space`, the same on all
three models.

## PLL frequency solve

`PllParameterReg::from_freq` solves the forward divider directly from the
target frequency rather than scanning, with the reference divider pinned
to 2 and the post-divider pair chosen within range. The hash-counting
number is derived from the same target. `frequency_range` bounds the
solve per chip; the upper bound is a uniform 1000 MHz local
tuning-policy envelope, not a chip limit.

## Tuners

- **`FixedTuner`** — the default. Drives the `ChipProtocol` bring-up
  through `board.chip`, anchors the open-core voltage, and walks the
  V²·f curve to the configured target on a 6.25 MHz grid.

## Verification

- `ubos-asic` code is pure and unit-tested under `src/tests/`; every
  register has a `REG_NUM` and an encode/decode round-trip test, and the
  bring-up recipes are guarded by `MockBus` op-sequence tests.
- Every crate path stays in the flake `checks.<system>` (`cargo check`
  plus `cargo clippy -- -D warnings`); `nix flake check` is the CI gate.
- Firmware RAM budgets are caught only by `cargo build --release` /
  the flake `*-build` derivations (`rp2040` is tightest), so watch
  binary size as per-chip recipes grow.
