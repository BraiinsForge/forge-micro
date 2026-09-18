# ASIC Mining Driver

Status: implemented — the code wins. When this file and the code
disagree, the code is right and this file needs a patch.

Reference for the hashboard driver: the chip support split across
`ubos-asic` (pure, sans-IO chip logic) and `ubos-mining` (the serial
I/O edge plus tuning policy). The Bitmain **BM1362 / BM1366 / BM1370**
family ships in the open `bm13xx` module behind a vendor-neutral seam;
the seam admits additional vendor drivers, which live in their own
crates depending on `ubos-asic` and are injected into the actor by the
consumer. Register bit layouts are documented separately in
[`REGISTERS.md`](REGISTERS.md); this document covers the architecture,
the bring-up sequence, and the per-chip differences.

## Crate responsibilities

- **`ubos-asic`** — pure and unit-testable, owns no UART and performs no
  direct I/O. Holds the vendor-neutral seam (`ChainDriver` /
  `Transport`), the `ChipDescriptor` / `HashboardDescriptor` data, and the
  `bm13xx` module (the Bitmain register layouts, wire framing, and
  `Bm1362` / `Bm1366` / `Bm1370` recipes) implementing `ChainDriver` as
  `Bm13xxDriver`. Other vendor drivers implement the same `ChainDriver`
  from their own crates; the consumer injects one concrete driver.
- **`ubos-mining`** — owns the per-chain serial actor (`Serial`) that
  drives the UART halves and implements `Transport` over them, the tuner that
  applies bring-up and the V²·f ramp, and the work dispatch / share
  verification path.

The dependency arrow is `ubos-mining → ubos-asic`. `ubos-asic` declares
`Transport`; `ubos-mining` implements it. This is the sans-IO rule: the
vendor driver frames and acts through a passed-in byte transport, so it
is testable against a mock transport, and the actor stays the single
I/O edge.

## The vendor-neutral seam

`ChainDriver` is the operations contract every vendor implements, sans-IO
over a `Transport` (`write_all` / `flush` / `read` / `set_baud` /
`delay_ms`):

- `reset_and_enumerate` / `configure` — bring the chain up to a safe-boot
  operating point on the fast UART; `configure` owns the baud switch.
- `set_frequency` — one ramp step, vendor-specific PLL programming.
- `send_work` — dispatch a neutral `WorkJob`; its consumer-assigned
  `work_id` is the bounded local registry index mapped onto the chip's echo
  field, matching BOS terminology. `WorkAssignment::assignment_id` correlates
  the concrete header with the stratum session's work ring, while `job_id`
  identifies the originating pool job. A hardware work ID is reused while its
  assignment stands, so a driver may skip writing a header the chain holds.
- `result_mode` + `ingest` (push) / `poll_results` (poll) — collect
  `ChainSolution`s. Push families stream nonce frames unsolicited; poll
  families read a nonce FIFO, so the actor branches its steady-state loop
  on `result_mode`, and the poll variant carries the period the family
  wants between two reads of one chip, which the actor's ticker takes.
  Bitmain is a push family.
- `reset` — re-arm the driver to its freshly-constructed state, keeping
  the chip identity; the actor calls it on a `Reset` command.
- `status_turn` / `pvt_sweep` — one status reading, or one pass over
  every chip, recorded into a `ChainPvt` the board owns. The
  driver picks the chip and the quantity; the actor sets the cadence,
  one turn per 480 ms of polling, and the tuner asks for the sweep right after
  `configure` so the thermal loop has a reading before the ramp. Both
  default to no-ops for a family whose chips carry no sensors.
  On a board whose chips are its only thermometer,
  `ubos_thermal::ChainTemperature` is the `TemperatureSensor` over those
  stores, one per chain. It reports the hottest die that any chip of any
  present chain has reported. It judges a chain and not one chip: a chip
  answers a turn only once per pass, which is minutes on a long chain. A
  chain is silent only if no chip of that chain has answered for 60
  seconds. It reads only the chains that `ChainStats` reports as present,
  because an absent connector has no chip to run a PVT block and must not
  halt the board. Each present chain's hottest die goes back into that
  chain's `ChainStats`, which is the temperature the API and the dashboard
  show per chain. The sensor publishes each chain's core voltage and
  writes a summary to the log from inside `sample`, so no task exists for
  reporting alone.

A poll family can fold its dispatch into the poll cycle. `send_work` then
stores the job and programs what keeps each chip apart from the others,
and `poll_results` writes the job to the chain after it has read every
nonce buffer. A chip whose buffer is one snapshot that a work write or an
update strobe replaces needs this order: a write between two reads
destroys the finds that were not read yet. Every other sweep ends with a
broadcast strobe, which hands over the finds without new work.

Some chips also search ntime on their own and report the offset they
chose. A host that moves ntime makes two headers of one job overlap, and
the chip re-finds the same solutions when the write restarts its scan. So
the driver writes work only when the pool assignment changes, and the
board passes `NtimeRoll::Chip` to the stratum setup, which publishes each
job once. Such a chip's search on one header repeats after a fixed number
of hashes, so the driver moves each chip's version slice on before that,
and the actor's work tick has no part in feeding the chain.

Share verification is vendor-neutral: the actor rebuilds the header from
the registry assignment and the driver-reported nonce / rolled version /
ntime offset via `ubos_asic::solution_hash`.

## The Bitmain family

The open `bm13xx` module implements `ChainDriver` for the Bitmain family:

| | `bm13xx` |
|---|---|
| Framing | `0x55 0xAA` preamble, CRC5 cmd / CRC16 work |
| Work | full header, 88-byte work frame |
| Solutions | pushed 11-byte frames |
| Freq→PLL | `PllParameterReg` calc |
| Fast baud | chip 3.125 M, host 3.0 M (BM1370: 520 k) |

A vendor whose framing, work model, or solution delivery differs
implements the same `ChainDriver` in its own crate — the seam's
`result_mode`, `boot_uart_rate`, and `work_slots` carry the varying
facts, and each driver's `configure` applies its own fast-baud switch
internally, so the actor drives every family through the one contract.

## Hardware model

A hashboard is a chain of chips in series. Power is organized into
**voltage domains**: a domain is one series element on the rail, and a
domain may contain more than one chip wired in parallel.

`HashboardDescriptor` carries the topology:

| Field | Meaning |
|-------|---------|
| `chip` | the `&'static ChipDescriptor` for the chip on the board |
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

The vendor-neutral seam in `ubos-asic` is `Transport` (byte I/O) +
`ChainDriver` (operations); see [The vendor-neutral
seam](#the-vendor-neutral-seam). Inside the `bm13xx` module the Bitmain
recipe keeps its own register-level internal seam, which the vendor
driver bridges onto `Transport`:

- **`ChipBus`** (bm13xx-internal) — the register I/O surface the recipe
  drives: `write_register_raw` / `read_register` / `send_command` /
  `delay_ms`, plus a provided typed `write_register<R>`. Register writes
  and commands are fire-and-forget, only reads can fail (returning an
  empty responder list).
- **`ChipProtocol`** (bm13xx-internal) — the per-chip bring-up recipe as
  plain imperative async steps. The steps are **default methods** keyed
  on `descriptor()`, because the recipe is identical across chip models
  at the level the driver models; a chip overrides only a step where it
  genuinely diverges.

```
ubos-asic  (sans-IO: no UART, no direct I/O)
  Transport trait            ← byte I/O seam; the consumer implements it
  ChainDriver trait          ← vendor operations (bring-up, work, results)
  ChipDescriptor             ← neutral per-chip facts the actor/tuner read
  bm13xx/                    ← Bitmain: registers, framing, ChipProtocol
      Bm13xxDriver             (bridges ChipProtocol onto Transport)
  (other vendor drivers implement ChainDriver from their own crates)
ubos-mining
  serial actors (one per chain) ← each owns one chain's UART halves +
                                   select loop; each is Transport and runs
                                   the concrete driver the consumer injects
  Miner                      ← owns one ChainHandle per chain; enumerates
                               every chain and retries a bad chip count
  tuner (policy)             ← drives ChainControl ops per chain; PSU +
                               V²·f ramp
```

### Chip selection

`ChainDriver` has async methods and is not `dyn`-compatible, so dispatch
is **static**: the consumer picks one concrete driver type and injects it
into the actor by value (`Serial::run` is generic over `C: ChainDriver`),
rather than a closed enum naming every vendor. Within `bm13xx`, the
`Chip` enum dispatches `ChipProtocol` across `Bm1362` / `Bm1366` /
`Bm1370` — a within-family enum, not a cross-vendor one.

Selection is a **compile-time property of the firmware**: the neutral
`HashboardDescriptor.chip` (a `&'static ChipDescriptor`) carries the chip
facts, and each firmware crate injects its driver from a `config::CHIP`
(e.g. `Bm13xxDriver::new(config::CHIP)`), so no shared type names every
vendor. A chain is always a single chip model. Each firmware's
`config::BOARD` / `config::CHIP` is the single point selection flows
from.

### The serial actor

A board owns a static array of `Serial`, one actor per chain. Each actor
owns its chain's UART read and write halves, implements `Transport` over
them, and runs a `select` loop over four arms: a work tick (periodic work
send via `ChainDriver::send_work`), a result arm (a UART read fed to
`ChainDriver::ingest` for push chips, or a poll ticker that
returns readiness for poll chips), a command channel (reset, reset-and-
enumerate, configure, set-frequency, start/stop work, terminate), and the
park signal. On dual-core boards every actor is pinned to Core 1 so the
network stack on Core 0 stays responsive; the shared session uses a
`CriticalSectionRawMutex`.

A selected poll chunk runs to completion outside the select. Its
`PollBatch` carries the snapshot's verification floor and says whether
more chips remain. The actor delivers the results before taking another
command or chunk. The driver retains its sweep cursor and closes the
snapshot only after the last chip. Commands and park requests take
priority between chunks; they never cancel a frame or discard a local
result batch. A park timeout shorter than a chunk fails closed.

The poll ticker skips missed ticks after a slow sweep. `poll_delay`
adds any remaining chip-specific wait before the next chunk, so a fast
sweep following a slow one cannot read a chip too early.

### Policy stays in the consumer

The recipe does no policy. PSU voltage, starting the work loop, and the
V²·f frequency ramp all live in the tuner. The host-side UART baud
switch lives in the driver: `configure` issues it through `Transport`
right after the chip-side `enable_fast_uart`.

### Testability

Recipes are exercised against a mock `ChipBus` that records every
call (`MockBus` in the `ubos-asic` tests). Tests assert the op sequence —
version rolling precedes the first read, baud is the last register
write, per-domain relay lands on the right chips, the gap count matches
the formula — with no hardware.

## Bring-up sequence (bm13xx)

`Bm13xxDriver` runs the `ChipProtocol` steps in order across
`reset_and_enumerate` and `configure`. Each step and the registers it
writes (layouts in [`REGISTERS.md`](REGISTERS.md)):

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
   no PLL first. `configure` switches the host UART to match immediately
   afterwards.
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

Every chain driver honors `WorkJob::version_mask`. On a
machine with more than one hashboard, the consumer splits the pool's
granted mask across chains before it builds a job: `partition_version_mask`
reserves the top bits of the grant to encode each chain's index as fixed
bits and gives every chain the same lower bits to roll, so sibling chains
never hash identical headers. The chip's driver reprograms
`MidstateCalReg`'s `version_rolled_bitmask` only when a job narrows the
mask it already has broadcast, so a machine with one hashboard, which
keeps the full grant, causes no extra register traffic. Reconstruction in
`rolled_version` merges only under the mask the chain actually rolled, so
it stays exact under a partial mask whichever way a chip echoes its
bits back: a chip of that kind returns only the rolled index, a chip that
hashes from version slots returns the whole field including the chain's
fixed bits, and both reconstruct the same header version.

### A pool that grants no version mask

The split needs as many bits of the grant as the chain index takes: two
bits for two or three chains. The client asks for `min-bit-count: 2` in
`mining.configure`, so a pool that supports version rolling grants
enough, and the split always works there.

A pool that does not support the extension omits the mask from its
reply, and the negotiated mask stays zero. There is then nothing to
split. Chain 0 keeps the empty grant and every other chain gets nothing
to roll, so every chain would hash the same headers from the same nonce
start and submit each share once per chain, which the pool counts as
duplicates.

A chain with nothing to roll is therefore left idle: `ubos-mining` skips
its work dispatch instead of sending it chain 0's job. The machine loses
no hashrate by this, because those chains could only repeat work chain 0
already does, and the pool sees one copy of each share rather than N.
`ubos-mining` logs the idle chain at error level, once per grant.

The version field is the only axis the chains are split on, and there is
no fallback inside it: a miner may vary only the bits the pool granted,
so a chain that moved a bit outside the grant would have the pool
rebuild a header the chain never hashed and reject the share. The way
out is to split `WorkJob::nonce_start` per chain instead, which the
field already carries to the wire. It is not done yet: the chips of one
chain already divide a 16-bit nonce offset among themselves through
`ChipNonceOffsetReg`, and whether a per-chain start composes with that
split or aliases against it has to be measured on the bench before the
work path can depend on it.

The chip distributes version-rolling work into a fixed number of
streams and tags each returned solution with a variant index in the low
bits of the echoed `work_id`. That count — the **work-id variant
count** — is 8 for BM1362/BM1366 and 16 for BM1370, carried as
`midstate_count` on the `BmDescriptor` statics in the crate-internal
`ubos_asic::bm13xx::chip`. The host:

- dispatches work-ids in strides of that count, so the chip's variant
  tags do not collide with the next job's base, and
- recovers the originating job for a returned solution by dividing its
  work-id by that count.

A wrong count misattributes solutions to the wrong job, so it is read
from the descriptor at runtime. The work registry is a fixed array sized
for the smallest count (`256 / 8 = 32` slots); a chip with a larger
count uses fewer slots.

Solutions are verified in place in the actor's `process_solution`:
`ubos_asic::solution_hash` rebuilds the 80-byte header with the rolled
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

- **`FixedTuner`** — the default. The `Miner` holds one `ChainHandle` per
  chain and owns enumeration: it resets and enumerates every chain,
  retrying a chain whose chip count does not match the board's expected
  count up to the retry budget before parking it. Once a chain is
  present, the tuner ramps it through `ChainControl` — `configure`,
  anchoring the open-core voltage, and walking the V²·f curve to the
  configured target on the chip's frequency-step grid.

## Verification

- `ubos-asic` code is pure and unit-tested in-crate: the vendor-neutral
  seam under `src/tests/` and the Bitmain module under `src/bm13xx/tests/`;
  every register has a `REG_NUM` and an encode/decode round-trip test, and
  the bring-up recipes are guarded by `MockBus` op-sequence tests.
- Every crate path stays in the flake `checks.<system>` (`cargo check`
  plus `cargo clippy -- -D warnings`); `nix flake check` is the CI gate.
- Firmware RAM budgets are caught only by `cargo build --release` /
  the flake `*-build` derivations (`rp2040` is tightest), so watch
  binary size as per-chip recipes grow.
