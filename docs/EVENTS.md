# Event Log

Status: implemented. Scope: the `ubos-events` crate, the per-subsystem `events!` catalogs, the `/api/system/events` HTTP surface, and the web-UI Events tab.

A user-facing record of the board's most important state transitions,
viewable from the web UI as a third top-bar tab after Dashboard and
Configuration. It is a separate mechanism from the developer
`defmt`-over-RTT logging: RTT needs a probe and is lost on every reset;
the event log is readable over HTTP with no probe and survives warm resets
(watchdog, software reset, panic path) on the boards that support retained
RAM.

## Terminology

"Logs" and "events" are kept strictly separate throughout this codebase.
**Logs** refer to the existing `defmt`/`ubos-logging` developer-facing RTT
facade: volatile, probe-only, high-rate developer tracing. **Events** refer
to this mechanism: curated user-facing state transitions, encoded into a
RAM ring, readable over HTTP with no probe, surviving warm resets. The web
UI tab is "Events". The crate is `ubos-events`. This mechanism is never
called "logs".

## Scope

The event log records self-explaining state transitions: "frequency ramp
started", "critical temperature, mining halted", "configuration changed",
"user paused mining". Per-event severity is declared in the catalog
(`info`, `warn`, `error`) and encoded in every record.

The event log is parallel to, not entangled with, `defmt`. One
`#[global_logger]` (`defmt_rtt`) already exists; the event log does not
replace or multiplex it. High-rate, per-step traces (every ramp hop) stay
on RTT only and never enter the event ring. The event log is not a metrics
store — Prometheus on `:8081` covers time-series gauges. Events are
discrete transitions, not samples. There is no log shipping or external
upload; the log is read-only over the local HTTP API.

Events are never persisted to on-board flash. See "Why no flash" below.

## Why no flash

The event log records to RAM only. On every dual-core board the
`ubos-mining` serial worker is pinned to core 1, and an `embassy-rp`
blocking flash erase/write must hand-shake core 1 to a stop. Per the
`docs/`-recorded OTA measurements, a normal write is ~41 ms but roughly
1 in 30 ops stalls **>15 s** in the `pause_core1` handshake under mining
load — past the 16.7 s hardware watchdog ceiling. Any flash-write path on
a running miner is therefore a latent mining-stall and watchdog-reset risk.

Avoiding flash also removes the need for a `sequential-storage` queue,
per-board `LOG_RANGE` constants, flash arbitration against the config
writer, boot replay, and the STM32F407 "no free sector" special case.

The cost is durability across a full power cycle. Retained RAM (below)
recovers most of it, surviving exactly the resets that matter most —
watchdog and crash — and losing only true power-off.

## Operating constraints

1. **No flash on the hot path.** Recording is a pure RAM operation.

2. **No RTC, but NTP wall clock is available.** `ubos_net::wall_clock_secs()`
   returns seconds since boot before NTP sync and Unix-epoch seconds after.
   The magnitude itself discriminates the two: a value obviously not in the
   current Unix-time range is pre-sync. Events carry a single `ts: u32`
   field written unconditionally; no separate synced flag and no backfill.
   `seq` carries monotonic ordering; `ts` is display-only.

3. **Retained RAM is per-board, not uniform.** The warm-reset survival
   layer is not the same mechanism on every target (see "Warm-reset
   survival"). The ring's logic is identical everywhere; only where its
   bytes live diverges.

## Architecture

One RAM ring behind one recording call. No flash, no second tier.

```
  any task ── events::critical_temperature(95) ──►  RAM ring (fixed capacity)
                       │                                      │
                       │ (paired defmt RTT line)              ├──► snapshotted by the HTTP endpoint
                       ▼                                      │
                   defmt_rtt                                  └──► lives in retained RAM where
                   (developer sink,                                available, re-validated at boot
                    unchanged)
```

- **RAM ring** — a fixed-capacity ring sized by `EVENT_LOG_CAPACITY` in a
  `static` behind `CriticalSectionRawMutex`, matching the `ASIC_SESSION`
  cross-core pattern. Every `events::` call lands here synchronously: no
  flash I/O, no blocking, no wear. This is what the HTTP endpoint serves.

- **Retained placement** — on boards with retained RAM the ring's backing
  storage is placed in a region the startup code does not zero, guarded by a
  validity header. At boot the firmware validates and adopts the existing
  ring instead of clearing it, so the UI shows the events that led up to a
  watchdog or crash reset. A cold power cycle leaves invalid retained
  memory, which the header check discards on the next boot.

## Recording model: encoded, deferred formatting

Records are encoded, not text. Formatting is deferred in the same spirit as
defmt: the record stores a stable code identifying a static format string
plus encoded numeric args; the human string is reconstructed in the
browser. The ring never stores formatted text. This is a first-party
scheme, not a reuse of defmt's machinery.

Each subsystem crate declares its events next to the code that fires them
using the `ubos_events::events!` `macro_rules!`:

```rust
// in e.g. ubos-thermal, beside the thermal task:
ubos_events::events! {
    critical_temperature(temp_c: i16)   error = "Critical temperature {} °C, mining halted",
    temperature_recovered(temp_c: i16)  info  = "Temperature recovered to {} °C, resuming",
    sensor_fault(misses: u32)           error = "Temperature sensor fault after {} misses",
}
```

The macro expands to `pub mod events` containing:

- **Typed emit functions** — `events::critical_temperature(95)` — which
  encode args, push the record into the global ring, and emit a paired
  `ubos_logging` line at the event's declared severity.
- `pub const CATALOG: &[EventDef]` — one entry per declared event,
  containing the stable code, severity, format string, and arg widths.

**Severity is per-event**, declared in the catalog. There is no
`event::info!/warn!/error!` macro family; severity belongs to the event's
identity and is not chosen at the call site.

The ring-vs-RTT decision is simply which function is called: `events::`
typed functions write an encoded record into the events ring **and** emit
a paired `defmt` RTT line for the developer. High-rate developer tracing
stays on `defmt::debug!`/`trace!` and never enters the ring.

## Interning and the code field

Each event's `code` is `const_fnv1a(slug)` — a compile-time FNV-1a 32-bit
hash of the event's function-name slug (e.g. `"critical_temperature"`),
computed by the `events!` macro at compile time. The hash is stable across
builds and OTA updates, so records written by old firmware decode correctly
under new firmware. A removed event's code renders as "unknown" in the
browser.

A host unit test asserts that all codes in the assembled catalog are
distinct, catching any collision before the code reaches a board.

## Decode table: served from device rodata

The browser decodes `code` + encoded args into human text against a small
first-party format convention: positional `{}` placeholders and a small
fixed set of per-arg integer types. The browser decoder is small and stable
because the format language is fixed.

The decode table is assembled from per-crate `CATALOG` slices at firmware
build time and served from device rodata. Firmware constructs:

```rust
const CATALOGS: &[&[ubos_events::EventDef]] = &[
    ubos_thermal::events::CATALOG,
    ubos_mining::events::CATALOG,
    // one line per subsystem crate
];
```

This slice is installed at boot via `ubos_events::set_catalogs` and served
by the `/catalog` handler. There is no post-link ELF extraction and no
hand-written central event list — only a one-line-per-module catalog list.
The table is automatically version-matched to the running firmware because
it is part of the same image. Format strings live in flash rodata; this
flash cost is acceptable on these boards.

The browser fetches the table once and caches it. Subsequent event fetches
supply only the encoded records; the browser renders them locally using the
cached table.

## Relationship to defmt

### Why the Events tab uses a first-party scheme

Capturing the defmt stream into RAM is technically feasible: a custom
`#[global_logger]` that tees to `defmt_rtt` and a RAM ring, with
per-message framing of defmt's variable-length output, filtering so
`debug!`/`trace!` call sites do not flood the ring. Delivering the string
table to the browser is also not a differentiator: defmt's strings are
stripped to the ELF, the event catalog's strings live in rodata, and both
end up served to the browser as a decode table.

The blocker is the browser decoder. Reusing defmt means owning a
version-pinned JS port of `defmt-decoder` covering defmt's full format
language (`{=u8}`, `{=[u8]}`, bitfields, nested `Format` derives). That
port breaks on defmt bumps. Constraining call sites "by convention" to a
scalar subset is fragile because any plain `defmt` macro anywhere in the
firmware can emit the full language into the same captured stream.

The first-party scheme avoids this entirely by defining a tiny fixed format
subset — positional `{}` plus a small integer arg set — and capturing only
`events::` call sites. The browser decoder is tens of lines and stable.

### The one reuse kept

`defmt_rtt` stays the developer sink. Every `events::` call emits both an
RTT line (developer-visible over a probe) and an encoded record into the
ring (user-visible over HTTP). The paired RTT line is emitted via
`ubos_logging` at the event's declared severity, so the common case needs no
separate log statement.

The two are nonetheless independent paths serving different audiences, and a
subsystem may keep an *additional* manual defmt log beside an `events::` call
at a transition. This is intentional, not redundant: the manual line can
carry richer developer detail the compact record drops — a float temperature
(`85.3 °C` versus the event's integer `85`), or operational narration
("forcing fan to 100%") — and may sit at a different severity than the event,
because the probe-side developer view and the HTTP-side operator log are
classified for their own audiences. For example `ubos-thermal` logs the
cooldown transition at `warn!` for the developer while recording
`temperature_recovered` as `info` for the operator. Such pairings are by
design; do not collapse them into the macro's line.

## The `ubos-events` crate

Under `ubos/`, `no_std`, depends on `ubos-logging`
(paired RTT line) and `embassy-sync` (`Mutex` + `CriticalSectionRawMutex`).
No `linkme` dependency and no proc-macro crate.

The crate is a strict leaf: it depends on nothing else in this workspace
(no `ubos-net`, no `ubos-config`), so the subsystem crates that emit
events can depend on it without creating dependency cycles. Two things are
injected by firmware at boot:

- the **backing ring buffer** (firmware owns the `static`, so per-board
  capacity and retained-RAM placement live in firmware);
- the **wall clock** (`ubos_net::wall_clock_secs`, injected so `ubos-events`
  need not depend on `ubos-net`).

```rust
// firmware boot, once:
static mut BACKING: ubos_events::RetainedRing<128> = ubos_events::RetainedRing::new();
// Safety: called once at boot before any task is spawned.
ubos_events::init(unsafe { &mut *core::ptr::addr_of_mut!(BACKING) }, ubos_net::wall_clock_secs);
```

Before `init` is called, `record()` is a no-op and `snapshot()` returns
zero, so host tests and early-boot calls are safe.

Flake gating: `clippy-ubos-events` (the check gate; `clippy -- -D warnings`
subsumes `cargo check`, matching the repo convention for library crates) and
`test-ubos-events` (host `cargo test`). Both are in the `fast-host`
aggregate. The single CI gate is `nix flake check` at the repo root.

Public surface:

- `const_fnv1a` — compile-time FNV-1a 32-bit hash of a string slice;
  used to derive stable event codes from function-name slugs.
- `Severity` — `#[repr(u8)]` enum: `Info = 0`, `Warn = 1`, `Error = 2`.
- `ArgWidth` — enum of supported arg widths: `U8`, `U16`, `U32`, `I8`,
  `I16`, `I32`; the decoder reads arg values from the payload by these widths.
- `EventDef` — static descriptor: `code: u32`, `severity: Severity`,
  `fmt: &'static str`, `args: &'static [ArgWidth]`.
- `Arg` — enum of encoded argument values (`U8(u8)`, `U16(u16)`, etc.);
  passed to `record()` and generated by the emit functions.
- `EventRecord` — the fixed-size `#[repr(C)]` in-RAM record (24 bytes);
  see "Record format" below. `EventRecord::EMPTY` is a const zero-value
  for use in `static` array initialisers.
- `RetainedRing<N>` — the firmware-owned backing: a `RetainedHeader`
  (magic/layout/seq/packed) plus `[EventRecord; N]`. `const fn new()` yields an
  all-zero (invalid) backing; firmware places it in retained RAM. See
  "Warm-reset survival".
- `Ring` — the internal operating view (`pub(crate)`) borrowing the header
  and entries; factored out so host tests exercise its logic without
  touching the global state.
- `init<const N: usize>(ring: &'static mut RetainedRing<N>, clock: fn() -> u64)`
  — validates the retained header and adopts or resets it, then installs the
  ring globally.
- `record(code: u32, severity: Severity, args: &[Arg])` — pushes one event
  into the global ring under the critical-section mutex. No-op before `init`.
- `snapshot(out: &mut [EventRecord]) -> usize` and
  `snapshot_after(out, after_seq) -> usize` — copy live records oldest-first;
  the HTTP handler streams with the `_after` cursor form. `latest_seq()` bounds
  a streamed read.
- `set_catalogs` / `catalogs` — install and read back the assembled
  per-crate `CATALOG` slices for the `/catalog` handler.
- `events!` — the `macro_rules!` that generates typed emit functions and
  `pub const CATALOG: &[EventDef]` for a subsystem crate.

### Record format

Fixed-size, `#[repr(C)]`, encoded. A fixed layout keeps the ring a plain
array, the JSON render trivial, and the retained backing a flat image that
survives a warm reset without serialisation.

| Field      | Type      | Bytes | Meaning                                          |
|------------|-----------|-------|--------------------------------------------------|
| `seq`      | u32       | 4     | monotonic; total order                           |
| `ts`       | u32       | 4     | `wall_clock_secs()` at the moment of recording   |
| `code`     | u32       | 4     | FNV-1a hash of the event's function-name slug    |
| `severity` | u8        | 1     | `Severity` cast to `u8`                          |
| `args`     | [u8; 11]  | 11    | encoded arg values, little-endian, no type tags  |

The record is `#[repr(C)]` with three `u32` fields, so its alignment is 4.
The total is `4+4+4+1+11 = 24` bytes, already a multiple of 4 — the
11-byte args array avoids padding. An 11-byte args array is free relative
to an 8-byte one: both round to a 24-byte record, so the three extra bytes
would otherwise be padding. At 128 entries the ring is `128 × 24 = 3 KiB`
plus the retained header.

A compile-time assertion `assert!(core::mem::size_of::<EventRecord>() == 24)`
is present in `lib.rs`.

### Timestamp: single field, no flag

A single `ts: u32` is written unconditionally from
`ubos_net::wall_clock_secs()`. Before NTP sync this returns seconds since
boot (small numbers, order 10²–10⁴). After sync it returns Unix epoch
seconds (order 1.7×10⁹). The magnitude discriminates the two — a value
that is obviously not a current-era Unix time is pre-sync — so there is no
separate synced flag and no separate uptime field. The browser shows
absolute local time when `ts` looks like a real Unix time, otherwise a
relative "before time sync" rendering. `seq` carries monotonic ordering;
`ts` is display-only.

`ubos_net::net::NTP_EPOCH_OFFSET` is nonzero once NTP sync completes and
is what `wall_clock_secs()` adds to the monotonic counter internally.

### Args encoding

Args are type-erased and encoded. Supported types are the six integer
widths in `ArgWidth`: `u8`, `u16`, `u32`, `i8`, `i16`, `i32`. A call site
may carry **at most 4 args**; in practice sites carry 0–2, occasionally 4
(the `ip_assigned` octets).

The `events!` macro knows each arg's type from the declaration, so the
record stores **raw little-endian values with no inline type tags**. Each
arg's type and the arg count live in the `EventDef.args` field, not in the
record; the browser reads widths from the defs table and parses the payload
accordingly. Values are packed in declaration order into the 11-byte
payload.

The only combination that overflows is four `u32` args (16 bytes); that
combination does not occur at any catalogued site, and overflow simply
truncates — consistent with the defmt convention for unbounded frames.

Inline short string args are not supported. No pointers may appear as args:
a `&str` arg cannot be stored as a pointer because it would dangle across a
reset or OTA and the browser cannot dereference device memory. If strings
are added later they must inline their bytes into the capped payload.

### `events!` macro grammar

```
ubos_events::events! {
    fn_name(arg: u16, …)  severity = "format string {}",
    …
}
```

`severity` is the bare keyword `info`, `warn`, or `error`. Each `ty` in the
arg list is one of `u8`, `u16`, `u32`, `i8`, `i16`, `i32`. Zero args is
valid (empty parentheses). A trailing comma after the last entry is accepted.

### Recording path

The recording path is encode-only. The code is already a compile-time
constant; under the critical-section mutex the path reads the injected
clock, encodes the args little-endian into the record, and pushes it into
the ring. The work held under the lock is a clock read and a handful of
integer copies, so the cross-core critical section stays short. No
formatting on the record path, no flash, never blocks. Formatting happens
entirely in the browser.

### Ring capacity: per-board config

Ring capacity is a per-board compile-time constant `EVENT_LOG_CAPACITY:
usize` placed on each board's `impl Board` in
`firmware/<board>/src/config.rs`, alongside the existing `WEBUI_HTTP_POOL`,
`API_HTTP_POOL`, and `Core1::STACK_BYTES` constants. It must be
compile-time because it sizes a `static [EventRecord; N]` array. The
RAM-rich boards (rp2350, bmm-pico, esp32s3) use 128; the rest (rp2040,
stm32f407, stm32f439) use 32. At 24 bytes per record that is 3 KiB and
768 B respectively. The rp2040 `*-build-release` flake derivation is the
RAM-overflow canary that confirms 32 fits.

| Board     | `EVENT_LOG_CAPACITY` | Ring size |
|-----------|----------------------|-----------|
| rp2350    | 128                  | 3 KiB     |
| bmm-pico  | 128                  | 3 KiB     |
| esp32s3   | 128                  | 3 KiB     |
| rp2040    | 32                   | 768 B     |
| stm32f407 | 32                   | 768 B     |
| stm32f439 | 32                   | 768 B     |

## Warm-reset survival

The ring's backing store is placed in memory the startup code does not
zero, guarded by a validity header so garbage from a cold boot is never
mistaken for a valid ring. The backing array is a firmware-owned `static`
that `init()` adopts.

### Mechanism is per-board

- **rp2350, bmm-pico** (RP2350A, cortex-m-rt): a `#[link_section =
  ".uninit"]` static. `cortex-m-rt` excludes `.uninit` from both `.bss`
  zeroing and `.data` init. The region is placed clear of the bootrom's
  boot-time SRAM scratch usage.
- **esp32s3** (Xtensa, esp-hal): there is no `cortex-m-rt` `.uninit`. The
  backing lives in RTC fast RAM, which is purpose-built to retain across
  resets and even deep sleep, via esp-hal's `#[ram(persistent)]` attribute.
- **rp2040, stm32f407, stm32f439** (cortex-m-rt): the same `#[link_section =
  ".uninit"]` static as rp2350/bmm-pico. Their SRAM is retained across a warm
  reset, so the ring survives there too.

| Board     | Survival region     | Mechanism                                     |
|-----------|---------------------|-----------------------------------------------|
| rp2350    | `.uninit` SRAM      | cortex-m-rt; clear of bootrom SRAM scratch    |
| bmm-pico  | `.uninit` SRAM      | same as rp2350                                |
| esp32s3   | RTC fast RAM        | esp-hal retained section; survives deep sleep |
| rp2040    | `.uninit` SRAM      | cortex-m-rt; retained across warm reset       |
| stm32f407 | `.uninit` SRAM      | cortex-m-rt; retained across warm reset       |
| stm32f439 | `.uninit` SRAM      | cortex-m-rt; retained across warm reset       |

### Validity guard

A magic word alone is not enough: a reset can land between two stores and
leave `head`/`len` torn while the magic word is still intact. The retained
header guards against this with a **single packed word**. The firmware owns
a `RetainedRing<N>` backing:

```rust
#[repr(C)]
struct RetainedHeader {
    magic:  u32,   // fixed sentinel
    layout: u32,   // binary layout stamp; bump when the header or EventRecord changes
    seq:    u32,   // monotonic counter, survives warm reset
    packed: u32,   // (len << 16) | head — written in one store
}
#[repr(C)]
struct RetainedRing<const N: usize> { header: RetainedHeader, entries: [EventRecord; N] }
```

`head` and `len` share `packed`, updated in a single store *after* the entry
is written, so the pair is never torn by a reset: a reset before that store
simply drops the in-flight entry, and the boot path always sees a consistent
position. At boot the ring is adopted only if `magic` matches **and** `layout`
equals the current build's stamp **and** `head`/`len` are in range; any failure
zeros and starts fresh.

There is no CRC: it would cost an O(len) recompute on every event and, worse,
a reset mid-CRC-write would discard the *whole* ring. The single packed store
gives torn-free indices without either cost. Cold-boot garbage that happens to
match magic, layout, and the ranges is ~2⁻⁶⁴ and harmless for a log.

The `layout` stamp is bumped only when the header or `EventRecord` layout
changes, not on every build. Because `code` is a stable FNV-1a hash, not a
link-assigned address, a post-OTA ring with matching layout decodes correctly
— a removed event's code renders as "unknown" in the browser. The ring may
therefore be kept across OTA, not just warm reset.

The retained `seq` counter stays monotonic across warm resets and only
restarts from zero on a cold boot.

### Reset taxonomy

- **Survives:** watchdog reset, software reset (`SYSRESETREQ` / esp-hal
  equivalent), brownout where the SRAM rail holds.
- **Lost (expected):** true power-off, deep brownout.
- **Panic is indirect.** `panic_probe` halts the core for a debugger; in
  the field with no probe the watchdog then fires and the resulting
  watchdog reset is what preserves the ring. Panic survival rides the
  watchdog path, not the panic handler — the log does not claim "captures
  panics" unconditionally.

## HTTP API

The event log replaces the former `/api/system/logs` route; conflating
events with logs would contradict the terminology boundary, so the old
`SystemLogs` route does not exist. Both endpoints below are session-gated
like the rest of `/api/system/*`.

### `GET /api/system/events`

Returns the encoded ring as JSON, oldest first. Each record carries `seq`,
`ts`, `sev`, `code` (the numeric hash), and the encoded `args` as hex.

Query parameter:

- `?since=<seq>` — return only records with `seq` greater than the given
  value; used for incremental fetch.

Example response shape:

```json
{
  "events": [
    { "seq": 411, "ts": 1718725790, "sev": "info",
      "code": 2947678903, "args": "9001000000000000000000" },
    { "seq": 412, "ts": 1718725800, "sev": "error",
      "code": 3251927560, "args": "5c00000000000000000000" }
  ]
}
```

`code` is the raw FNV-1a hash and `args` is the 11-byte payload as hex; the
browser resolves the code against the cached defs table and reads only the
arg widths that entry declares. The body is **streamed** close-delimited
(`Connection: close`, no `Content-Length`): the handler captures the latest
`seq`, then reads the ring in small `seq`-cursor batches and writes each
record straight to the socket. No buffer scales with the ring capacity, so a
full 128-entry ring needs no large per-connection allocation. The browser
sorts by `seq` for display.

### `GET /api/system/events/catalog`

Serves the assembled per-crate `CATALOG` slices as a JSON array of
`{code, sev, fmt, args}`, one entry per catalogued event. The browser
fetches it once at tab load and caches it; external tools may fetch it to
decode events offline. The array is empty until firmware installs catalogs
at boot via `ubos_events::set_catalogs`; the handler reads them back through
`ubos_events::catalogs`, so no `dispatch()` parameter is needed.

## Web UI

Source in `ubos/ubos-webui/web/`, built and gzipped at compile
time by `build.rs`, served from flash on port 80.

Routing is hash-based, not a toggle in the page markup: `viewOf` in
`web/app.tsx` maps the `#events` hash to the events view, `TopNav.tsx`
renders the matching top-bar link, and `App` swaps in
`<Events />` (`web/components/Events.tsx`) on a `hashchange`.

`Events.tsx` fetches `GET /api/system/events/catalog` once per mount and
caches the `{code, sev, fmt, args}` defs; `load(incremental)` then fetches
`GET /api/system/events`, adding `?since=<seq>` when incremental. Each
record is decoded against its catalog def — `decodeArgs` unpacks the
positional args from the hex payload, `formatMessage` substitutes them
into the `{}` template — and merged into a `Map` keyed by `seq`, so an
incremental fetch appends rather than replaces. The table sorts
newest-first by time or severity, both toggleable, into a
severity-coloured row (time, severity badge, message); a reload button
re-runs `load(false)` and, while the tab is mounted, a 60 s timer runs
`load(true)` for the same incremental fetch. A higher-than-expected
first `seq` on an incremental fetch — the ring overwrote the gap before
the fetch — is tracked in component state; nothing renders it yet.

Table and severity-badge styles live in `web/styles/index.css`
(`.evtab`, `.badge`); the top-bar link reuses the existing nav styles.

The existing CSRF/auth handling need no change; the gzip
`Content-Encoding` firmware already serves needs none either.

### Why polling, not streaming

The Events tab polls (a 60 s timer plus the manual reload icon, both
incremental via `?since=<seq>`) rather than holding a stream open.
Server-Sent Events and WebSockets each require a held-open connection per
listener, and connections are the scarce resource here: the API server runs
only two workers, and a past "unreachable for minutes" bug was caused by
connection-pool starvation when TCP connections were not released. One open
Events tab on SSE would pin a worker for its entire lifetime. Incremental
polling delivers only the new events with no held connection and no worker
pinned.

## Catalogued events

The shipped catalog is 28 events across seven subsystem crates. Each is
declared in that crate's `events! { … }` block beside the code that fires
it; the live set is also served from `GET /api/system/events/catalog`. The
`mining_halted_*` family is one event per pause cause rather than a single
parameterised event, so each carries no args.

| Crate          | Event slug                            | Severity | Args                    | Message                                       |
|----------------|---------------------------------------|----------|-------------------------|-----------------------------------------------|
| `ubos-config`  | `config_changed`                      | info     | `u32, u32`              | `Configuration changed: {} MHz, {} mV`        |
| `ubos-config`  | `factory_reset`                       | warn     | —                       | `Factory reset to defaults`                   |
| `ubos-config`  | `network_applied`                     | info     | —                       | `Network config applied`                      |
| `ubos-config`  | `network_reverted`                    | warn     | —                       | `Network config reverted`                     |
| `ubos-thermal` | `critical_temperature`                | error    | `i16`                   | `Critical temperature {} °C, mining halted`   |
| `ubos-thermal` | `temperature_recovered`               | info     | `i16`                   | `Temperature recovered to {} °C, resuming`    |
| `ubos-thermal` | `sensor_fault`                        | error    | `u32`                   | `Temperature sensor fault after {} misses`    |
| `ubos-thermal` | `sensor_recovered`                    | info     | `u32`                   | `Temperature sensor recovered after {} misses`|
| `ubos-manager` | `mining_halted_dangerous_temperature` | warn     | —                       | `Mining halted: dangerous temperature`        |
| `ubos-manager` | `mining_halted_broken_fan`            | warn     | —                       | `Mining halted: broken fan`                   |
| `ubos-manager` | `mining_halted_user_request`          | warn     | —                       | `Mining halted: user request`                 |
| `ubos-manager` | `mining_halted_pool_unreachable`      | warn     | —                       | `Mining halted: pool unreachable`             |
| `ubos-manager` | `mining_halted_ota_update`            | warn     | —                       | `Mining halted: OTA update`                   |
| `ubos-manager` | `mining_halted_tuner_fault`           | warn     | —                       | `Mining halted: tuner fault`                  |
| `ubos-manager` | `mining_resumed`                      | info     | —                       | `Mining resumed`                              |
| `ubos-psu`     | `psu_fault`                           | error    | —                       | `PSU fault`                                   |
| `ubos-mining`  | `miner_reset`                         | info     | —                       | `Miner reset`                                 |
| `ubos-mining`  | `psu_enabled`                         | info     | —                       | `PSU enabled`                                 |
| `ubos-mining`  | `tuner_initialised`                   | info     | —                       | `Tuner initialised`                           |
| `ubos-mining`  | `tuner_fault`                         | error    | —                       | `Tuner fault`                                 |
| `ubos-mining`  | `chip_count_mismatch`                 | error    | `u8, u8`                | `Chip count {} differs from expected {}`      |
| `ubos-mining`  | `frequency_ramp_started`              | info     | `u32`                   | `Frequency ramp started to {} kHz`            |
| `ubos-mining`  | `frequency_ramp_complete`             | info     | `u32`                   | `Frequency ramp complete at {} kHz`           |
| `ubos-net`     | `ip_assigned`                         | info     | `u8, u8, u8, u8`        | `IP assigned {}.{}.{}.{}`                      |
| `stratum-client` | `pool_unreachable`                  | warn     | —                       | `Pool unreachable`                            |
| `ubos-api`     | `user_paused`                         | info     | —                       | `User paused mining`                          |
| `ubos-api`     | `user_resumed`                        | info     | —                       | `User resumed mining`                         |
| `ubos-api`     | `user_restart`                        | warn     | —                       | `User-requested restart`                      |

Per-hop frequency/voltage traces stay on `defmt::debug!` and never enter the
ring.

## Verification

- **`ubos-events` host unit tests:** record encode/decode round-trip for
  each `ArgWidth`, ring wrap-around at capacity, `const_fnv1a`
  determinism, a catalog-uniqueness test (all codes in a fixed catalog are
  distinct), and `init` not-yet-called → `record` is a no-op. Run on
  host (`std`), gated by the `test-ubos-events` flake derivation.
- **Flake gates:** `clippy-ubos-events` (`clippy -- -D warnings`, the
  check gate for library crates in this repo) and `test-ubos-events`
  (`cargo test`), both in the `fast-host` aggregate. A crate absent from
  `checks` is not gated by CI; the single CI gate is `nix flake check`
  at the repo root.
- **Per-board derivations:** the existing `cargo build` derivations
  exercise the call sites. The rp2040 `*-build-release` derivation is the
  RAM-overflow canary for the ring's capacity.
- **Manual:** flash rp2350 or bmm-pico, drive a config change and a
  thermal trip, trigger a watchdog or software reset, and confirm events
  survive and render in the Events tab. Confirm a cold power cycle clears
  them. Repeat on esp32s3 against RTC RAM.
