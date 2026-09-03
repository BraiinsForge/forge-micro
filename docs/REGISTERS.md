# BM1362 Register Reference

Status: implemented — the code wins. When this file and the code
disagree, the code is right and this file needs a patch.

This document describes the typed BM1362 register set in
`ubos_asic::bm13xx::registers`. Each section gives the register address,
direction, and field layout. Field names are kept raw where no richer
semantic name is established for the bits.

## Register inventory

27 registers are typed today. Nine are reads (chip → host) and the
rest are writes (host → chip). Address order:

| Addr | Struct | Direction |
|------|--------|-----------|
| `0x00` | `ChipIdReg` | read |
| `0x08` | `PllParameterReg` | write |
| `0x0c` | `ChipNonceOffsetReg` | write |
| `0x10` | `HashCountingNumberReg` | write |
| `0x14` | `TicketMaskReg` | write |
| `0x18` | `MiscCtrlReg` | write |
| `0x28` | `BaudRateReg` | write |
| `0x2c` | `UartRelayReg` | write |
| `0x3c` | `CoreControlReg` | write |
| `0x40` | `CoreRegisterValueReg` | read |
| `0x44` | `ExternalTemperatureSensorReg` | read |
| `0x4c` | `NonceErrorCounterReg` | read |
| `0x54` | `AnalogMuxControl` | write |
| `0x58` | `IoDriverStrengthCtrlReg` | write |
| `0x60` | `Pll1ParameterReg` | write |
| `0x68` | `Pll3ParameterReg` | write |
| `0x70` | `Pll0DividerReg` | write |
| `0x74` | `Pll1DividerReg` | write |
| `0x7c` | `Pll3DividerReg` | write |
| `0x88` | `Domain0NonceCounterReg` | read |
| `0x89` | `Domain1NonceCounterReg` | read |
| `0x8a` | `Domain2NonceCounterReg` | read |
| `0x8b` | `Domain3NonceCounterReg` | read |
| `0x8c` | `TotalNonceCounterReg` | read |
| `0xa4` | `MidstateCalReg` | write |
| `0xa8` | `SoftResetControlReg` | write |
| `0xb9` | `MiscSettingsReg` | write |

---

## Per-register detail

### `0x00` — `ChipIdReg`

Chip-identity register. Read-only; the chip reports its model
identifier, hash-core count, and on-chain address.

| Bits | Field | Purpose |
|------|-------|---------|
| 0..8 | `address` | On-chain address assigned during enumeration |
| 8..16 | `core_num` | Number of hashing cores reported by the chip |
| 16..32 | `chip_id` | Model identifier (`0x1362` for BM1362) |

---

### `0x08` — `PllParameterReg`

PLL0 parameter register. Programs the integer-N PLL that derives the
hash clock from the 25 MHz crystal, with the reference divider pinned
to 2:
`freq_mhz = 25 * fbdiv / 2 / ((postdiv1_minus_1 + 1) * (postdiv2_minus_1 + 1))`.

| Bits | Field | Purpose |
|------|-------|---------|
| 0..3 | `postdiv2_minus_1` | Post-divider 2 stored value (`actual_pd2 - 1`) |
| 4..7 | `postdiv1_minus_1` | Post-divider 1 stored value (`actual_pd1 - 1`) |
| 8..16 | `refdiv` | Reference divider, pinned to `0x02` (see `PLL_REFDIV_VALUE`) |
| 16..24 | `fbdiv` | Forward divider; VCO output is `25 MHz * fbdiv / 2` |
| 28 | `vco_high` | VCO high-frequency band select (set at/above `PLL_VCO_HIGH_THRESHOLD_HZ`, 2400 MHz) |
| 30 | `pll_en` | PLL enable bit |
| 31 | `locked` | PLL lock-detector output, read-only |

`from_freq` sets `vco_high` from the chosen FBDIV: the band edge is at
`VCO = 25 MHz * fbdiv / 2 >= 2400 MHz`. The same layout repeats at
addresses `0x60` / `0x64` / `0x68` for PLL1 / PLL2 / PLL3.

---

### `0x0c` — `ChipNonceOffsetReg`

Per-chip nonce-offset register. Splits the 32-bit nonce space across
the chain so each chip searches a disjoint sub-range. With `cnov` set,
the chip derives its initial nonce from `offset`; cleared, it falls
back to the address-derived default. The exact offset-to-nonce mapping
is chip-internal and not modeled here.

| Bits | Field | Purpose |
|------|-------|---------|
| 0..16 | `offset` | Per-chip nonce offset |
| 31 | `cnov` | Chip-nonce-offset valid |

---

### `0x10` — `HashCountingNumberReg`

Hash-counting-number register. An opaque 32-bit HCN payload. The local
calculator computes it inversely from the target hash clock:
`hcn = floor(HCN_NUMERATOR_KHZ / freq_khz)` with
`HCN_NUMERATOR_KHZ = 5 * 2^24 * 1000 = 83_886_080_000`. This exact
numerator is a local derivation.

| Bits | Field | Purpose |
|------|-------|---------|
| 0..32 | `hcn` | Hash-counting number |

**Formula note.** The general local form is `hcn ∝ 1 / freq`. The kHz
form preserves precision for the 6.25 MHz PLL steps the calculator
emits. The reference corpus in `tests/registers.rs` validates every
25 MHz step in 50..=500 MHz against this formula.

---

### `0x14` — `TicketMaskReg`

Chip-side share filter. The chip only returns a nonce when its hash
matches the mask. The wire encoding is
`(difficulty - 1).reverse_bits().swap_bytes()`; difficulty must be a
power of two so the mask is contiguous high bits.

| Bits | Field | Purpose |
|------|-------|---------|
| 0..32 | `ticket_mask` | Raw mask payload (opaque) |

**Cross-register note:** the same encoding repeats at address `0x38`
for the second-stage filter (`TicketMask2`); not currently typed in
this crate.

---

### `0x18` — `MiscCtrlReg`

Misc control register. Carries baud-rate divisor, pin-function
selectors, and other chip-level mode bits.
`MISC_CTRL_DEFAULT_BRINGUP = 0xb000_c100` captures the value chips
are programmed with at bring-up.

| Bits | Field | Purpose |
|------|-------|---------|
| 0..4 | `unknown_bits_3_0` | Opaque |
| 4..6 | `tfs` | TFS pin function selector |
| 6 | `reserved_bit_6` | Reserved |
| 7 | `unknown_bit_7` | Opaque |
| 8..13 | `baud_div` | Baud divisor (low-speed UART path) |
| 13 | `unknown_bit_13` | Opaque |
| 14 | `rfs` | RFS pin function selector |
| 15 | `unknown_bit_15` | Opaque |
| 16 | `unknown_bit_16` | Opaque (V2 groups bits 19:16 as raw `B19_16`) |
| 17..22 | `unknown_bits_21_17` | Opaque |
| 22 | `unknown_bit_22` | Opaque |
| 23..28 | `unknown_bits_27_23` | Opaque |
| 28..32 | `unknown_bits_31_28` | Opaque |

---

### `0x28` — `BaudRateReg`

Fast-UART configuration register. Programs the high-speed UART
divisor and clock-source bits:
`baud = uart_clk / ((bt8d + 1) * 8)`.
`BAUDRATE_FAST_3MBAUD = 0x1130_0000` is the current fast-path preset.
Fields without a known name are intentionally raw.

| Bits | Field | Purpose |
|------|-------|---------|
| 0..4 | `raw_bits_3_0` | Raw preserved bits |
| 4 | `raw_bit_4` | Raw preserved bit |
| 5..7 | `raw_bits_6_5` | Raw preserved bits |
| 7 | `raw_bit_7` | Raw preserved bit |
| 8..16 | `bt8d` | Baud-tick-8 divider |
| 16 | `raw_bit_16` | Raw preserved bit |
| 17 | `raw_bit_17` | Raw preserved bit |
| 18..20 | `raw_bits_19_18` | Raw preserved bits |
| 20..24 | `pll1_div4` | PLL1 divider field |
| 24..26 | `raw_bits_25_24` | Raw preserved bits |
| 26 | `bclk_sel` | Baud clock source (`BCK_SEL`) |
| 27 | `raw_bit_27` | Raw preserved bit |
| 28 | `b28` | Raw bit B28 |
| 29 | `raw_bit_29` | Raw preserved bit |
| 30 | `raw_bit_30` | Raw preserved bit |
| 31 | `raw_bit_31` | Raw preserved bit |

---

### `0x2c` — `UartRelayReg`

UART relay configuration for multi-chip chains. When a chain spans
more than one domain boundary, the host programs the first and last
chip of each domain so the UART signal is relayed across the boundary.
`relays_enabled(gap_cnt)` sets `gap_cnt` and enables both relays.
Single-chip deployments leave this register unprogrammed.

| Bits | Field | Purpose |
|------|-------|---------|
| 0 | `co_rel_en` | Command-out relay enable |
| 1 | `ro_rel_en` | Response-out relay enable |
| 2..16 | `unknown_bits_15_2` | Opaque |
| 16..32 | `gap_cnt` | Inter-byte gap count inserted across the boundary |

---

### `0x3c` — `CoreControlReg`

Per-core register access. Drives the chip's per-core register file:
each write enqueues a read or write of a `CoreRegister` inside one
or more cores. Read responses are returned via `CoreRegisterValueReg`
at address `0x40`.

| Bits | Field | Purpose |
|------|-------|---------|
| 0..8 | `reg_wdata` | Data byte (writes) / ignored (reads) |
| 8..13 | `reg_addr` | Target per-core register (`CoreRegister` enum) |
| 13 | `reserved_bit_13` | Reserved |
| 14 | `unknown_bit_14` | Opaque |
| 15 | `reg_wr` | Direction: `true` writes, `false` reads |
| 16..24 | `core_id` | Origin core ID |
| 24..31 | `unknown_bits_30_24` | Opaque |
| 31 | `do_cmd` | High command bit (`DO_CMD`) |

The `CoreRegister` enum names the 4 IDs the firmware writes
(`ClockDelayCtrl=0x00`, `CoreReg2=0x02`, `HashClockCtrl=0x05`,
`CoreRegB=0x0b`).

---

### `0x40` — `CoreRegisterValueReg`

Per-core register response. Read-back path for `CoreControlReg` read
operations. Bits 15:8 are the `FOUND` byte; this document keeps that
byte raw rather than inferring a returned core-register ID.

| Bits | Field | Purpose |
|------|-------|---------|
| 0..8 | `core_reg_value` | Per-core register value |
| 8..16 | `found` | `FOUND` byte |
| 16..24 | `core_id` | Originating core ID |

---

### `0x44` — `ExternalTemperatureSensorReg`

External temperature-sensor read register. Reports on-die and
external temperature-diode ADC results. Exposed as an opaque 32-bit
value.

| Bits | Field | Purpose |
|------|-------|---------|
| 0..32 | `value` | Opaque ADC payload |

---

### `0x4c` — `NonceErrorCounterReg`

Nonce-error counter. Cumulative count of nonce errors detected by
the chip. Read by the host to monitor chip error rate.

| Bits | Field | Purpose |
|------|-------|---------|
| 0..32 | `count` | Cumulative nonce-error count |

---

### `0x54` — `AnalogMuxControl`

Analog mux control. Selects which on-die diode or supply rail the
analog mux exposes for temperature / voltage measurement. The
corresponding ADC result is read from `ExternalTemperatureSensorReg`
at address `0x44`. `ANALOG_MUX_DISABLED = 0x03` shuts the mux off.

| Bits | Field | Purpose |
|------|-------|---------|
| 0..32 | `ana_mux_sel` | Mux selector code |

---

### `0x58` — `IoDriverStrengthCtrlReg`

I/O driver-strength control. 4-bit drive-strength field per output
pin plus return-driver enables and RF drive strength.
`IO_DRIVER_STRENGTH_DEFAULT = 0x0001_1111` captures the BM1362
default (every low strength `0x1`, high fields zero).

| Bits | Field | Purpose |
|------|-------|---------|
| 0..4 | `co_ds` | Command / TX output drive strength |
| 4..8 | `bo_ds` | Busy output drive strength |
| 8..12 | `nrsto_ds` | Reset output drive strength |
| 12..16 | `clko_ds` | Clock output drive strength |
| 16..20 | `ro_ds` | Response / RX output drive strength |
| 20 | `d0rs_en` | Domain-0 return-driver enable |
| 21 | `d1rs_en` | Domain-1 return-driver enable |
| 22 | `d2rs_en` | Domain-2 return-driver enable |
| 23 | `d3rs_en` | Domain-3 return-driver enable |
| 24..28 | `rf_ds` | RF output drive strength |
| 28..32 | `reserved_high` | Reserved |

---

### `0x70` — `Pll0DividerReg`

PLL0 divider register. Programs four 4-bit `W` divider bytes that
feed PLL0. On BM1362 only the low byte is programmed; the upper
three bytes are written zero. `PLL0_DIVIDER_RESET = 0x0` is the
value written before each PLL parameter update.

| Bits | Field | Purpose |
|------|-------|---------|
| 0..8 | `wdiv` | PLL0 lane-0 W divider (applied as `wdiv + 1`) |
| 8..16 | `unknown_bits_15_8` | Lane-1 divider (not used by BM1362) |
| 16..24 | `unknown_bits_23_16` | Lane-2 divider (not used by BM1362) |
| 24..32 | `unknown_bits_31_24` | Lane-3 divider (not used by BM1362) |

The same 4-byte W-divider layout repeats at addresses
`0x74` / `0x78` / `0x7c` for PLL1 / PLL2 / PLL3.

---

### `0x88` / `0x89` / `0x8a` / `0x8b` — Per-domain nonce counters

Per-domain returned-nonce / share counters, one register per power
domain. Read by the host to monitor per-domain throughput.

| Bits | Field | Purpose |
|------|-------|---------|
| 0..32 | `count` | Cumulative nonce count for the domain |

---

### `0x8c` — `TotalNonceCounterReg`

BM1366-family total nonce / share counter. Cumulative count of
returned nonces across all four power domains. This address carries a
different meaning in other BM13xx family layouts, so keep this tied to
the BM1366/BM1362 path.

| Bits | Field | Purpose |
|------|-------|---------|
| 0..32 | `count` | Cumulative aggregate nonce count |

---

### `0xa4` — `MidstateCalReg`

Midstate calibration register. Configures version rolling: which
version-field bits the chip may toggle, plus the `EN` and `EN2` enable
bits. `MIDSTATE_CAL_BRINGUP_BASELINE = 0x9000_FFFF` captures the
production preset.

| Bits | Field | Purpose |
|------|-------|---------|
| 0..16 | `version_rolled_bitmask` | Per-bit version-rolling mask |
| 16..28 | `reserved_mid` | Reserved (BM1362 preset leaves zero) |
| 28 | `en2` | Second enable bit (`EN2`) |
| 29 | `unknown_bit_29` | Opaque |
| 30 | `unknown_bit_30` | Opaque |
| 31 | `en` | Primary enable bit (`EN`) |

---

### `0xa8` — `SoftResetControlReg`

Soft-reset control. Carries the raw reset selector fields; the
register is treated as opaque raw nibbles plus raw bits B8 and B10, so
no per-core or per-domain reset semantics are asserted.
`SOFT_RESET_QUIESCENT = 0x0` is the quiescent value;
`SOFT_RESET_SRST_B1 = 0x02` sets bit 1 of the low selector nibble.

| Bits | Field | Purpose |
|------|-------|---------|
| 0..4 | `b3_0` | Raw low selector nibble |
| 4..8 | `b7_4` | Raw selector nibble (bits 7:4) |
| 8 | `b8` | Raw bit B8 |
| 9 | `b9` | Raw preserved bit |
| 10 | `b10` | Raw bit B10 |
| 11..16 | `reserved_mid` | Reserved |
| 16..24 | `b23_16` | Raw initialization byte |
| 24..32 | `reserved_high` | Reserved (preset leaves zero) |

---

### `0xb9` — `MiscSettingsReg`

Opaque misc-settings register whose field semantics are not
characterized. The BM1370 bring-up writes the fixed constant
`MISC_SETTINGS_BM1370_BRINGUP = 0x0000_4480`; the BM1362 does not
require it. Exposed as a raw `u32` so a bring-up recipe can write the
known constant without claiming a field layout it cannot verify.

| Bits | Field | Purpose |
|------|-------|---------|
| 0..32 | `value` | Opaque misc-settings payload |

---

## Shared field structures

Several field structures repeat across multiple register addresses on
the BM1362.

### PLL parameter — repeats at four addresses

The 8-bit `fbdiv`, 3-bit `postdiv1_minus_1`, 3-bit `postdiv2_minus_1`,
`refdiv`, and enable/lock bits documented for
`PllParameterReg` (`0x08`) repeat verbatim at:

- `0x60` PLL1 Parameter (`Pll1ParameterReg`)
- `0x64` PLL2 Parameter
- `0x68` PLL3 Parameter (`Pll3ParameterReg`)

PLL1 drives the UART clock on BM1366 and PLL3 on BM1370; each is a thin
newtype wrapping `PllParameterReg` with its own `REG_NUM`, so a bring-up
recipe writes them through the typed `write_register` seam.

### PLL divider — repeats at four addresses

The 4×4-bit W-divider lane layout documented for `Pll0DividerReg`
(`0x70`) repeats at:

- `0x74` PLL1 Divider (`Pll1DividerReg`)
- `0x78` PLL2 Divider
- `0x7c` PLL3 Divider (`Pll3DividerReg`)

### Ticket-mask encoding — repeats at two addresses

The `(difficulty - 1).reverse_bits().swap_bytes()` encoding
documented for `TicketMaskReg` (`0x14`) repeats at `0x38`
(`TicketMask2`, second-stage filter).

### Opaque `u32` counter pattern

Seven registers in our typed set are transparent `u32` newtypes (no
field decomposition):

- `ExternalTemperatureSensorReg` (`0x44`)
- `NonceErrorCounterReg` (`0x4c`)
- `Domain0NonceCounterReg` / `Domain1NonceCounterReg` / `Domain2NonceCounterReg` / `Domain3NonceCounterReg` (`0x88..=0x8b`)
- `TotalNonceCounterReg` (`0x8c`)

### `CoreRegister` enum — single use site

The `CoreRegister` enum (`ClockDelayCtrl=0x00`, `CoreReg2=0x02`,
`HashClockCtrl=0x05`, `CoreRegB=0x0b`) is a field of
`CoreControlReg` (`0x3c`, write). It is not used by
`CoreRegisterValueReg` (`0x40`, read response), whose bits 15:8 are
the `FOUND` byte.

---

## What is intentionally NOT typed

These addresses exist in the BM1362 register map but are not currently
exposed in our typed register set. Most are write-only configuration
applied once at bring-up, or read-only counters not needed yet:

- `0x04` HashRate / Time1sCounter
- `0x1c` I2CControl — family docs exist; not used by the current board
- `0x20` OrderedClockEnable — not written by the BM1366/BM1370 bring-up
- `0x38` TicketMask2 — second-stage share filter; layout shared with
  `TicketMaskReg`
- `0x48` ErrorFlag — opaque error status register
- `0x5c` TimeOut — chip-internal work-timeout
- `0x64` PLL2 Parameter / `0x78` PLL2 Divider — layout shared with
  `PllParameterReg` / `Pll0DividerReg`; no instance constant defined
  (PLL2 is not used for the hash or UART clock)
- `0x80` / `0x84` ClockOrderControl0 / ClockOrderControl1

The PLL1 / PLL3 parameter and divider instances (`0x60` / `0x68` /
`0x74` / `0x7c`) are typed: `Pll1ParameterReg` / `Pll3ParameterReg` wrap
`PllParameterReg` and `Pll1DividerReg` / `Pll3DividerReg` wrap
`Pll0DividerReg`, each with its own `REG_NUM`.

Adding any of these is a mechanical port of the relevant register
layout.
