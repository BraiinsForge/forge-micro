// Copyright (C) 2026  Braiins Systems s.r.o.
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.
//
// Braiins Systems s.r.o. reserves the right to grant any party a license
// to this program, or any part thereof, under any terms, and such a grant
// shall be considered distinct from the grant above.

//! User-facing event log for ubos firmware.
//!
//! Provides a fixed-capacity RAM ring of encoded event records, readable
//! over HTTP without a probe attached. This is a separate mechanism from
//! `defmt`/RTT developer logging: RTT requires a probe and is lost on
//! every reset; the event ring is read over HTTP and survives warm resets
//! on boards that place the backing buffer in retained RAM.
//!
//! # Design rationale
//!
//! "Logs" and "events" are kept strictly separate. Logs are the
//! `defmt`/`ubos-logging` developer facade: volatile, probe-only,
//! high-rate tracing. Events are curated user-facing state transitions
//! with per-event severity declared in the catalog, never chosen at the
//! call site. High-rate traces (every ramp hop) stay on RTT and never
//! enter the ring. An `events::` call also emits a paired RTT line, but
//! a subsystem may keep an additional manual `defmt` line beside it when
//! the developer view needs richer detail or a different severity; such
//! pairings are deliberate, so do not collapse them.
//!
//! The ring never touches flash. On dual-core boards a blocking flash
//! write must hand-shake the mining core to a stop, and under load that
//! handshake has been measured to stall past the hardware watchdog
//! ceiling, so any flash write on a running miner risks a watchdog
//! reset. The cost is durability across a true power-off; retained RAM
//! recovers the resets that matter most (watchdog, software reset,
//! crash-then-watchdog).
//!
//! The HTTP endpoint polls rather than streams. Server-Sent Events and
//! `WebSockets` each hold a connection open per listener, and connections
//! are scarce here: the API server runs only two workers, and a past
//! "unreachable for minutes" bug traced to connection-pool starvation
//! when TCP connections were not released. One open Events tab on SSE
//! would pin a worker for its entire lifetime; incremental polling via
//! `?since=<seq>` delivers only new events with no held connection and
//! no worker pinned.
//!
//! Records are encoded, not text, in the same spirit as `defmt`: the
//! record stores a stable code plus raw little-endian args, and the
//! browser formats them against the catalog served from device rodata.
//! The scheme is first-party rather than a defmt capture because reusing
//! defmt would mean owning a version-pinned browser port of
//! `defmt-decoder` covering defmt's whole format language; the
//! first-party format is positional `{}` over six integer widths, so the
//! browser decoder is tens of lines and stable. The code is a
//! compile-time FNV-1a hash of the event's slug, stable across builds
//! and OTA, so a ring written by old firmware still decodes; a removed
//! event renders as unknown.
//!
//! Every record carries one `ts: u32` from the injected wall clock:
//! seconds since boot before NTP sync, Unix-epoch seconds after. The
//! magnitude discriminates the two, so there is no synced flag and no
//! backfill. `seq` carries the total order; `ts` is display-only.
//!
//! # Usage
//!
//! Declare events in a subsystem crate with the [`events!`] macro:
//!
//! ```rust
//! ubos_events::events! {
//!     thermal_limit(temp_c: u16)  warn  = "Thermal limit {} °C",
//!     sensor_fault()              error = "Temperature sensor fault",
//! }
//! events::thermal_limit(95);
//! events::sensor_fault();
//! ```
//!
//! Firmware owns the backing [`RetainedRing`] and initialises the ring once at
//! boot. Placing the backing in retained RAM (a `.uninit` section on cortex-m,
//! RTC fast RAM on esp32s3) lets the ring survive a warm reset:
//!
//! ```rust,ignore
//! static mut BACKING: ubos_events::RetainedRing<128> = ubos_events::RetainedRing::new();
//! // Safety: called once at boot before any task is spawned.
//! ubos_events::init(unsafe { &mut *core::ptr::addr_of_mut!(BACKING) }, ubos_net::wall_clock_secs);
//! ```
//!
//! Before [`init`] is called, [`record`] is a no-op and [`snapshot_after`]
//! returns zero.

#![cfg_attr(not(feature = "std"), no_std)]

use core::cell::RefCell;

use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::once_lock::OnceLock;

pub use ubos_logging;

#[cfg(test)]
#[path = "tests/codec.rs"]
mod tests_codec;

#[cfg(test)]
#[path = "tests/ring.rs"]
mod tests_ring;

#[cfg(test)]
#[path = "tests/catalog.rs"]
mod tests_catalog;

const _: () = assert!(core::mem::size_of::<EventRecord>() == 24);

/// Computes a stable FNV-1a 32-bit hash of a string at compile time.
///
/// Used to derive event codes from function-name slugs. The hash is
/// stable across builds and OTA updates, so records written by old
/// firmware decode correctly under new firmware.
#[must_use]
pub const fn const_fnv1a(s: &str) -> u32 {
    let bytes = s.as_bytes();
    let mut hash: u32 = 0x811c_9dc5;
    let mut index = 0;
    while index < bytes.len() {
        hash ^= bytes[index] as u32;
        hash = hash.wrapping_mul(0x0100_0193);
        index += 1;
    }
    hash
}

/// Severity of an event, encoded as a `u8` in [`EventRecord`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Severity {
    /// Informational state transition, no action required.
    Info = 0,
    /// Condition that warrants attention but did not stop mining.
    Warn = 1,
    /// Condition that caused mining to stop or a fault to occur.
    Error = 2,
}

/// Width of a single argument as stored in the [`EventRecord`] payload.
///
/// The decoder reads arg values from the payload using these widths,
/// so the record itself carries no per-arg type tags.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ArgWidth {
    /// Unsigned 8-bit integer.
    U8,
    /// Unsigned 16-bit integer.
    U16,
    /// Unsigned 32-bit integer.
    U32,
    /// Signed 8-bit integer.
    I8,
    /// Signed 16-bit integer.
    I16,
    /// Signed 32-bit integer.
    I32,
}

/// Maximum byte length of an event format string.
///
/// The `/api/system/events/catalog` route in `ubos-api` serialises each
/// [`EventDef`] into a fixed `DEF_JSON_MAX`-byte (256) buffer. Roughly 88
/// bytes of that budget is fixed punctuation plus the code, severity, and
/// arg-width strings, leaving 168 bytes for the escaped `fmt`. A longer
/// `fmt` would be silently truncated into invalid JSON, so the [`events!`]
/// macro asserts every declared `fmt` against this bound at compile time.
/// The ceiling sits below the raw 168-byte limit so a handful of escaped
/// `"`/`\` characters cannot push the serialised form over budget.
pub const MAX_FMT_LEN: usize = 160;

/// Static descriptor for one event type, used by the HTTP `/catalog` endpoint.
///
/// One `EventDef` is generated per entry by the [`events!`] macro and
/// collected into a `pub static CATALOG` slice in the generated `events`
/// module. Firmware assembles per-crate catalogs into a single registry
/// served to the browser.
#[derive(Clone, Copy, Debug)]
pub struct EventDef {
    /// Stable FNV-1a hash of the event's function-name slug.
    pub code: u32,
    /// Declared severity of this event.
    pub severity: Severity,
    /// Positional `{}` format string rendered by the browser.
    pub fmt: &'static str,
    /// Argument widths in declaration order, used by the browser decoder.
    pub args: &'static [ArgWidth],
}

/// A single encoded argument value.
///
/// Passed to [`record`] (and the generated emit functions) to encode
/// integer arguments into the 11-byte payload of an [`EventRecord`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Arg {
    /// Unsigned 8-bit value.
    U8(u8),
    /// Unsigned 16-bit value.
    U16(u16),
    /// Unsigned 32-bit value.
    U32(u32),
    /// Signed 8-bit value.
    I8(i8),
    /// Signed 16-bit value.
    I16(i16),
    /// Signed 32-bit value.
    I32(i32),
}

/// A single encoded event record stored in the RAM ring.
///
/// Fixed at 24 bytes (`#[repr(C)]`). The layout is three `u32` fields
/// (alignment 4), one `u8` severity, and an 11-byte args payload. The
/// 11-byte payload avoids padding: `4+4+4+1+11 = 24`, which is already
/// a multiple of 4.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(C)]
pub struct EventRecord {
    /// Monotonic sequence number assigned by the ring. Total ordering.
    pub seq: u32,
    /// `wall_clock_secs()` at the moment the event was recorded.
    pub ts: u32,
    /// Stable FNV-1a hash of the event's function-name slug.
    pub code: u32,
    /// `Severity` cast to `u8`.
    pub severity: u8,
    /// Encoded argument values, little-endian, no type tags.
    pub args: [u8; 11],
}

impl EventRecord {
    /// Constant zero-initialised record for use in `static` array initialisers.
    pub const EMPTY: EventRecord = EventRecord {
        seq: 0,
        ts: 0,
        code: 0,
        severity: 0,
        args: [0u8; 11],
    };
}

/// Encodes a slice of [`Arg`] values little-endian into an 11-byte payload.
///
/// Values are packed in declaration order. If a value would overflow the
/// remaining bytes it is silently truncated. Truncation does not occur at
/// any catalogued call site (the heaviest valid combination is two `u32`
/// and one `u16`, totalling 10 bytes).
pub(crate) fn encode_args(args: &[Arg]) -> [u8; 11] {
    let mut payload = [0u8; 11];
    let mut offset = 0usize;

    // Appends the little-endian bytes if they fit in what remains. A value
    // that would overflow is skipped, leaving offset untouched. Signed
    // values keep their two's-complement bit pattern, which the decoder reads
    // back at the declared width, so no information is lost.
    let mut put = |bytes: &[u8]| {
        if let Some(slot) = payload.get_mut(offset..offset + bytes.len()) {
            slot.copy_from_slice(bytes);
            offset += bytes.len();
        }
    };

    for arg in args {
        match arg {
            Arg::U8(value) => put(&[*value]),
            Arg::I8(value) => put(&value.to_le_bytes()),
            Arg::U16(value) => put(&value.to_le_bytes()),
            Arg::I16(value) => put(&value.to_le_bytes()),
            Arg::U32(value) => put(&value.to_le_bytes()),
            Arg::I32(value) => put(&value.to_le_bytes()),
        }
    }

    payload
}

/// Sentinel marking a header written by this firmware. A mismatch means cold
/// boot (uninitialised retained RAM) or garbage, so the boot path starts fresh.
const MAGIC: u32 = 0x5542_4556;

/// Binary-layout stamp. Bump when [`RetainedHeader`] or [`EventRecord`] changes
/// shape so a post-OTA build never adopts a stale-layout ring.
const LAYOUT_VERSION: u32 = 1;

/// The ring's mutable header, kept beside the entries in retained RAM so the
/// write position and sequence survive a warm reset.
///
/// `head` and `len` share one `packed` word so the boot path can never observe
/// a torn pair: a reset can land between two stores but not mid-store, and the
/// push path updates `packed` in a single write after the entry is in place.
#[repr(C)]
pub struct RetainedHeader {
    magic: u32,
    layout: u32,
    seq: u32,
    packed: u32,
}

impl RetainedHeader {
    /// A valid empty header, the state a fresh or reset ring starts from.
    const FRESH: RetainedHeader = RetainedHeader {
        magic: MAGIC,
        layout: LAYOUT_VERSION,
        seq: 0,
        packed: 0,
    };

    const fn head(&self) -> usize {
        (self.packed & 0xFFFF) as usize
    }

    const fn len(&self) -> usize {
        (self.packed >> 16) as usize
    }

    fn set_head_len(&mut self, head: usize, len: usize) {
        // Single store keeps the pair torn-free across a reset. head and len
        // are bounded by the ring capacity (at most 65535), so the casts to
        // u32 cannot lose data.
        #[allow(clippy::cast_possible_truncation)]
        let packed = ((len as u32) << 16) | (head as u32 & 0xFFFF);
        self.packed = packed;
    }

    /// True when this header was written by a matching-layout build and its
    /// indices fit the backing of `capacity` entries.
    fn is_valid(&self, capacity: usize) -> bool {
        self.magic == MAGIC
            && self.layout == LAYOUT_VERSION
            && self.len() <= capacity
            && self.head() < capacity
    }
}

/// Firmware-owned ring backing: the header plus a fixed entry array. Placed in
/// retained RAM on boards that support it (see the firmware `EVENT_LOG_BACKING`
/// statics) so the ring survives warm resets; plain RAM elsewhere.
#[repr(C)]
pub struct RetainedRing<const N: usize> {
    header: RetainedHeader,
    entries: [EventRecord; N],
}

impl<const N: usize> RetainedRing<N> {
    /// All-zero backing. The zero header has `magic == 0`, so the boot path
    /// treats it as invalid and resets it; this is also what cold-boot
    /// retained RAM and a freshly zeroed volatile static look like.
    /// Compile-time bound: head and len each pack into 16 bits of the header
    /// word, so the capacity must be non-zero and fit in 16 bits. Referenced
    /// from `new` so every backing forces the check at monomorphisation.
    const CAPACITY_VALID: () = assert!(
        N > 0 && N <= 0xFFFF,
        "RetainedRing capacity must be 1..=65535"
    );

    #[must_use]
    pub const fn new() -> Self {
        let () = Self::CAPACITY_VALID;
        Self {
            header: RetainedHeader {
                magic: 0,
                layout: 0,
                seq: 0,
                packed: 0,
            },
            entries: [EventRecord::EMPTY; N],
        }
    }

    /// Discards any prior contents and starts an empty, valid ring.
    ///
    /// Only the header is rewritten; stale entry bytes are left in place
    /// because every read gates on the validated `len`, so no caller can
    /// observe an entry past the live region.
    fn reset(&mut self) {
        self.header = RetainedHeader::FRESH;
    }
}

impl<const N: usize> Default for RetainedRing<N> {
    fn default() -> Self {
        Self::new()
    }
}

/// Operating view over a [`RetainedRing`]: borrows the retained header and
/// entries plus the volatile clock.
///
/// Factored out of the global so its logic can be exercised in host unit tests
/// without touching the global `RING` state. `head`/`len`/`seq` live in the
/// borrowed header, so a ring rebuilt over retained memory after a warm reset
/// resumes where it left off.
pub(crate) struct Ring {
    /// Retained header carrying the write position and sequence.
    header: &'static mut RetainedHeader,
    /// Retained entry storage owned by firmware.
    entries: &'static mut [EventRecord],
    /// Clock returning seconds since boot or Unix epoch. Matches
    /// `ubos_net::wall_clock_secs`, so firmware passes it directly.
    clock: fn() -> u64,
}

impl Ring {
    /// Pushes a new event into the ring, overwriting the oldest on overflow.
    pub(crate) fn push(&mut self, code: u32, severity: Severity, args: &[Arg]) {
        let capacity = self.entries.len();
        if capacity == 0 {
            return;
        }
        let head = self.header.head();
        let len = self.header.len();
        // Wrapping is intentional. Saturating would instead freeze every later
        // record at u32::MAX and stall every cursor permanently. The trade-off:
        // once every ~4 billion events the seq returns to a small value, and a
        // streaming client whose cursor sat near u32::MAX then receives nothing
        // until it notices the backward jump and resets its cursor. At device
        // event rates this wrap is never reached in practice.
        self.header.seq = self.header.seq.wrapping_add(1);
        // Unix seconds fit in u32 until 2106; truncating the wall clock is intended.
        #[allow(clippy::cast_possible_truncation)]
        let ts = (self.clock)() as u32;
        let payload = encode_args(args);

        self.entries[head] = EventRecord {
            seq: self.header.seq,
            ts,
            code,
            severity: severity as u8,
            args: payload,
        };

        // Publish the new position last, in one store, so a reset before this
        // point only drops the in-flight entry rather than corrupting the ring.
        let new_head = (head + 1) % capacity;
        let new_len = if len < capacity { len + 1 } else { len };
        self.header.set_head_len(new_head, new_len);
    }

    /// Copies live records with `seq` greater than `after_seq` into `out`,
    /// oldest-first, returning the count copied.
    ///
    /// Records carry monotonically increasing sequence numbers, so callers
    /// stream the whole ring without a large buffer by passing the highest
    /// `seq` they have seen as `after_seq` on each call.
    pub(crate) fn snapshot_after(&self, out: &mut [EventRecord], after_seq: u32) -> usize {
        let capacity = self.entries.len();
        let len = self.header.len();
        if capacity == 0 || len == 0 {
            return 0;
        }
        let oldest = if len < capacity {
            0
        } else {
            self.header.head()
        };
        let mut written = 0;
        let mut index = 0;
        while index < len && written < out.len() {
            let record = self.entries[(oldest + index) % capacity];
            if record.seq > after_seq {
                out[written] = record;
                written += 1;
            }
            index += 1;
        }
        written
    }

    /// Returns the sequence number of the most recently recorded event, or
    /// zero when the ring is empty.
    pub(crate) fn latest_seq(&self) -> u32 {
        self.header.seq
    }
}

/// Global ring guarded by a critical-section blocking mutex.
///
/// `RefCell` is required because `blocking_mutex::Mutex` only yields `&T`
/// inside the closure, so interior mutability is needed to write the ring.
/// The `RefCell` is only accessed from within a critical section, which
/// prevents concurrent borrows on single-core and serialises them across
/// cores via the critical-section implementation.
static RING: Mutex<CriticalSectionRawMutex, RefCell<Option<Ring>>> = Mutex::new(RefCell::new(None));

/// Initialises the event ring from a firmware-owned [`RetainedRing`] and clock.
///
/// Must be called once at boot, before any task is spawned, so that subsequent
/// calls to [`record`] have a ring to write into. Calling [`record`] or
/// [`snapshot_after`] before `init` is safe and produces a no-op or zero
/// respectively.
///
/// If `ring` holds a valid header from a prior session on this build (matching
/// `magic`/`layout`, in-range indices) it is adopted, so events that preceded a
/// warm reset are kept; otherwise it is reset to empty. On boards whose backing
/// is plain RAM the header is zeroed every boot, so this always resets.
pub fn init<const N: usize>(ring: &'static mut RetainedRing<N>, clock: fn() -> u64) {
    if !ring.header.is_valid(N) {
        ring.reset();
    }
    let RetainedRing { header, entries } = ring;
    let entries: &'static mut [EventRecord] = entries;
    RING.lock(|cell| {
        let mut slot = cell.borrow_mut();
        // Install once. A stray second call keeps the first ring rather than
        // overwriting the global with a second borrow of the backing, so a
        // double init cannot corrupt the live state.
        if slot.is_none() {
            *slot = Some(Ring {
                header,
                entries,
                clock,
            });
        }
    });
}

/// Records an event into the global ring.
///
/// No-op if [`init`] has not been called. Never blocks, never allocates,
/// and never writes flash. Safe to call from any task or interrupt handler.
pub fn record(code: u32, severity: Severity, args: &[Arg]) {
    RING.lock(|cell| {
        if let Some(ring) = cell.borrow_mut().as_mut() {
            ring.push(code, severity, args);
        }
    });
}

/// Copies live records with `seq` greater than `after_seq` into `out`,
/// oldest-first, returning the count copied. Zero before [`init`] or when no
/// record is newer than `after_seq`.
pub fn snapshot_after(out: &mut [EventRecord], after_seq: u32) -> usize {
    RING.lock(|cell| {
        cell.borrow()
            .as_ref()
            .map_or(0, |ring| ring.snapshot_after(out, after_seq))
    })
}

/// Returns the highest sequence number recorded so far, or zero before
/// [`init`] or when the ring is empty. Used to bound a streamed read to the
/// records that existed when the read began.
#[must_use]
pub fn latest_seq() -> u32 {
    RING.lock(|cell| cell.borrow().as_ref().map_or(0, Ring::latest_seq))
}

/// The per-crate event catalogs, injected by firmware at boot.
///
/// Each subsystem crate's `events!` macro emits a `CATALOG`; firmware
/// assembles those into one `&[&[EventDef]]` and installs it here so the
/// `/catalog` HTTP endpoint can serve the decode table. It is an empty slice
/// until `set_catalogs` is called, so the endpoint returns an empty list
/// rather than failing before boot wires it up.
///
/// A `OnceLock` rather than a `Mutex`, because the slot is written once and
/// read many times, and `catalogs()` should not pay a critical section for
/// every read of a value that never changes after boot. `OnceLock::init`
/// stores the value and then marks the slot ready with a relaxed store, and
/// `try_get` reads that flag relaxed too, so a relaxed store does not by
/// itself publish the value to another core. That is safe here only because
/// every board starts its second core after `set_catalogs` runs, and a core
/// start orders memory between the two cores.
static CATALOGS: OnceLock<&'static [&'static [EventDef]]> = OnceLock::new();

/// Installs the assembled per-crate catalogs. Called once at boot.
///
/// Installs once: a stray second call keeps the first catalogs rather than
/// replacing them, mirroring [`init`]'s install-once discipline. `init`
/// returns `Err` on a second call; that error is deliberately discarded.
pub fn set_catalogs(catalogs: &'static [&'static [EventDef]]) {
    let _ = CATALOGS.init(catalogs);
}

/// Returns the installed catalogs, or an empty slice before `set_catalogs`.
///
/// Uses `try_get`, not `get`: `get` returns a future that busy-polls the
/// executor until the slot is filled, which is only useful for a value
/// installed after other tasks start awaiting it. `try_get` is a single
/// relaxed load and returns immediately either way.
#[must_use]
pub fn catalogs() -> &'static [&'static [EventDef]] {
    CATALOGS.try_get().copied().unwrap_or(&[])
}

/// Returns `true` if every event `code` across all catalogs in `registry` is
/// unique. Event codes are 32-bit FNV-1a hashes of function-name slugs drawn
/// from several crates and assembled at the firmware site; a collision would
/// silently merge two events in the decode table. Being `const`, this is meant
/// for a compile-time `assert!` where the assembled registry is visible.
#[must_use]
pub const fn codes_unique(registry: &[&[EventDef]]) -> bool {
    let mut catalog = 0;
    while catalog < registry.len() {
        let events = registry[catalog];
        let mut index = 0;
        while index < events.len() {
            if count_code(registry, events[index].code) > 1 {
                return false;
            }
            index += 1;
        }
        catalog += 1;
    }
    true
}

/// Counts how many events across all catalogs in `registry` carry `code`.
const fn count_code(registry: &[&[EventDef]], code: u32) -> usize {
    let mut total = 0;
    let mut catalog = 0;
    while catalog < registry.len() {
        let events = registry[catalog];
        let mut index = 0;
        while index < events.len() {
            if events[index].code == code {
                total += 1;
            }
            index += 1;
        }
        catalog += 1;
    }
    total
}

/// Maps a type token to the corresponding [`ArgWidth`] variant.
///
/// This is an internal helper invoked by the [`events!`] macro.
#[doc(hidden)]
#[macro_export]
macro_rules! __arg_width {
    (u8) => {
        $crate::ArgWidth::U8
    };
    (u16) => {
        $crate::ArgWidth::U16
    };
    (u32) => {
        $crate::ArgWidth::U32
    };
    (i8) => {
        $crate::ArgWidth::I8
    };
    (i16) => {
        $crate::ArgWidth::I16
    };
    (i32) => {
        $crate::ArgWidth::I32
    };
}

/// Wraps a value expression in the corresponding [`Arg`] variant.
///
/// This is an internal helper invoked by the [`events!`] macro.
#[doc(hidden)]
#[macro_export]
macro_rules! __arg_value {
    (u8,  $val:expr) => {
        $crate::Arg::U8($val)
    };
    (u16, $val:expr) => {
        $crate::Arg::U16($val)
    };
    (u32, $val:expr) => {
        $crate::Arg::U32($val)
    };
    (i8,  $val:expr) => {
        $crate::Arg::I8($val)
    };
    (i16, $val:expr) => {
        $crate::Arg::I16($val)
    };
    (i32, $val:expr) => {
        $crate::Arg::I32($val)
    };
}

/// Maps the severity keyword to the [`Severity`] variant.
///
/// This is an internal helper invoked by the [`events!`] macro.
#[doc(hidden)]
#[macro_export]
macro_rules! __severity {
    (info) => {
        $crate::Severity::Info
    };
    (warn) => {
        $crate::Severity::Warn
    };
    (error) => {
        $crate::Severity::Error
    };
}

/// Maps the severity keyword to the matching `ubos_logging` macro name.
///
/// This is an internal helper invoked by the [`events!`] macro.
#[doc(hidden)]
#[macro_export]
macro_rules! __log_sev {
    (info,  $fmt:expr $(, $arg:expr)*) => { $crate::ubos_logging::info!($fmt $(, $arg)*)  };
    (warn,  $fmt:expr $(, $arg:expr)*) => { $crate::ubos_logging::warn!($fmt $(, $arg)*)  };
    (error, $fmt:expr $(, $arg:expr)*) => { $crate::ubos_logging::error!($fmt $(, $arg)*) };
}

/// Declares a group of typed events, generating typed emit functions and a catalog.
///
/// # Syntax
///
/// ```rust,ignore
/// ubos_events::events! {
///     fn_name(arg0: u16, arg1: u32) warn  = "format string {} {}",
///     fn_name2()                    error = "no-arg format",
/// }
/// ```
///
/// `severity` is one of `info`, `warn`, or `error`. Each `ty` is one of
/// `u8`, `u16`, `u32`, `i8`, `i16`, `i32`.
///
/// # Output
///
/// Expands to `pub mod events { ... }` containing:
/// - `pub const CATALOG: &[EventDef]` — one entry per declared event.
/// - One `pub fn fn_name(arg0: ty0, ...)` per entry that calls [`record`]
///   and emits a paired `ubos_logging` line at the declared severity.
///
/// The paired log line covers the common case, but the event ring and the
/// `defmt`/RTT log are independent paths for different audiences. A subsystem
/// may keep an additional manual log beside an `events::` call to carry richer
/// developer detail (a float value, operational narration) or a different
/// severity than the event; such pairings are intentional, not redundant.
#[macro_export]
macro_rules! events {
    (
        $(
            $fn_name:ident ( $($arg_name:ident : $arg_type:tt),* )
                $sev:ident = $fmt:literal
        ),* $(,)?
    ) => {
        pub mod events {
            #[allow(unused_imports)]
            use super::*;

            /// Event definitions for the enclosing module, one entry per declared event.
            ///
            /// A `const`, not a `static`, so firmware can aggregate the
            /// per-crate catalogs into one `&[&[EventDef]]` in a `const`
            /// initialiser; a `static` referring to another `static` is rejected.
            pub const CATALOG: &[$crate::EventDef] = &[
                $(
                    $crate::EventDef {
                        code:     $crate::const_fnv1a(stringify!($fn_name)),
                        severity: $crate::__severity!($sev),
                        fmt:      $fmt,
                        args:     &[ $( $crate::__arg_width!($arg_type) ),* ],
                    },
                )*
            ];

            // The `/catalog` route serialises each `fmt` into a fixed-size
            // buffer; a string over budget would emit truncated, invalid JSON.
            $(
                const _: () = assert!(
                    $fmt.len() <= $crate::MAX_FMT_LEN,
                    "event fmt string exceeds MAX_FMT_LEN catalog JSON budget"
                );
            )*

            $(
                #[doc = concat!("Records a `", stringify!($fn_name), "` event into the global ring and emits a paired log line.")]
                pub fn $fn_name( $($arg_name: $arg_type),* ) {
                    $crate::record(
                        $crate::const_fnv1a(stringify!($fn_name)),
                        $crate::__severity!($sev),
                        &[ $( $crate::__arg_value!($arg_type, $arg_name) ),* ],
                    );
                    $crate::__log_sev!($sev, $fmt $(, $arg_name)*);
                }
            )*
        }
    };
}
