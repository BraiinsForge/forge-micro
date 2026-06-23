//! User-facing event log for ubos firmware.
//!
//! Provides a fixed-capacity RAM ring of encoded event records, readable
//! over HTTP without a probe attached. This is a separate mechanism from
//! `defmt`/RTT developer logging: RTT requires a probe and is lost on
//! every reset; the event ring is read over HTTP and survives warm resets
//! on boards that place the backing buffer in retained RAM.
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
//! Firmware initialises the ring once at boot:
//!
//! ```rust,ignore
//! static mut BACKING: [ubos_events::EventRecord; 128] =
//!     [ubos_events::EventRecord::EMPTY; 128];
//! // Safety: called once at boot before any task is spawned.
//! ubos_events::init(unsafe { &mut BACKING }, ubos_net::wall_clock_secs);
//! ```
//!
//! Before [`init`] is called, [`record`] is a no-op and [`snapshot`]
//! returns zero.

#![cfg_attr(not(feature = "std"), no_std)]

use core::cell::RefCell;

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::blocking_mutex::Mutex;

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

// ── Ring ──────────────────────────────────────────────────────────────────────

/// Fixed-capacity event ring operating on a borrowed static slice.
///
/// Factored out of the global so its logic can be exercised in host unit
/// tests without touching the global `RING` state.
pub(crate) struct Ring {
    /// Backing storage, borrowed from a `'static` array owned by firmware.
    buf: &'static mut [EventRecord],
    /// Index of the next write slot (wraps modulo `buf.len()`).
    head: usize,
    /// Number of valid entries, at most `buf.len()`.
    len: usize,
    /// Sequence number of the last assigned record; first record gets 1.
    seq: u32,
    /// Clock returning seconds since boot or Unix epoch. Matches
    /// `ubos_net::wall_clock_secs`, so firmware passes it directly.
    clock: fn() -> u64,
}

impl Ring {
    /// Constructs a ring over `buf` using `clock` for timestamps.
    pub(crate) fn new(buf: &'static mut [EventRecord], clock: fn() -> u64) -> Self {
        Self {
            buf,
            head: 0,
            len: 0,
            seq: 0,
            clock,
        }
    }

    /// Pushes a new event into the ring, overwriting the oldest on overflow.
    pub(crate) fn push(&mut self, code: u32, severity: Severity, args: &[Arg]) {
        let capacity = self.buf.len();
        if capacity == 0 {
            return;
        }
        self.seq = self.seq.saturating_add(1);
        // Unix seconds fit in u32 until 2106; truncating the wall clock is intended.
        #[allow(clippy::cast_possible_truncation)]
        let ts = (self.clock)() as u32;
        let payload = encode_args(args);

        self.buf[self.head] = EventRecord {
            seq: self.seq,
            ts,
            code,
            severity: severity as u8,
            args: payload,
        };

        self.head = (self.head + 1) % capacity;
        if self.len < capacity {
            self.len += 1;
        }
    }

    /// Copies live records oldest-first into `out`, returning the count copied.
    ///
    /// At most `min(self.len, out.len())` records are written. When the ring
    /// has wrapped, the oldest record is at `self.head`.
    pub(crate) fn snapshot(&self, out: &mut [EventRecord]) -> usize {
        let capacity = self.buf.len();
        if capacity == 0 || self.len == 0 {
            return 0;
        }
        let count = self.len.min(out.len());
        // When len < capacity the ring has not wrapped and oldest is at index 0.
        // When len == capacity the ring has wrapped and oldest is at self.head.
        let oldest = if self.len < capacity { 0 } else { self.head };

        for (index, slot) in out.iter_mut().enumerate().take(count) {
            *slot = self.buf[(oldest + index) % capacity];
        }
        count
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

/// Initialises the event ring with a firmware-owned backing buffer and clock.
///
/// Must be called once at boot, before any task is spawned, so that
/// subsequent calls to [`record`] have a ring to write into. Calling
/// [`record`] or [`snapshot`] before `init` is safe and produces a no-op
/// or zero respectively.
pub fn init(buf: &'static mut [EventRecord], clock: fn() -> u64) {
    RING.lock(|cell| {
        *cell.borrow_mut() = Some(Ring::new(buf, clock));
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

/// Copies a snapshot of the live ring into `out`, oldest-first.
///
/// Returns the number of records written. Returns zero if [`init`] has not
/// been called or the ring is empty.
pub fn snapshot(out: &mut [EventRecord]) -> usize {
    RING.lock(|cell| cell.borrow().as_ref().map_or(0, |ring| ring.snapshot(out)))
}

// ── Catalog registry ──────────────────────────────────────────────────────────

/// The per-crate event catalogs, injected by firmware at boot.
///
/// Each subsystem crate's `events!` macro emits a `CATALOG`; firmware
/// assembles those into one `&[&[EventDef]]` and installs it here so the
/// `/catalog` HTTP endpoint can serve the decode table. It is an empty slice
/// until `set_catalogs` is called, so the endpoint returns an empty list
/// rather than failing before boot wires it up.
static CATALOGS: Mutex<CriticalSectionRawMutex, RefCell<&'static [&'static [EventDef]]>> =
    Mutex::new(RefCell::new(&[]));

/// Installs the assembled per-crate catalogs. Called once at boot.
///
/// Installs once: a stray second call keeps the first catalogs rather than
/// replacing them, mirroring [`init`]'s install-once discipline.
pub fn set_catalogs(catalogs: &'static [&'static [EventDef]]) {
    CATALOGS.lock(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.is_empty() {
            *slot = catalogs;
        }
    });
}

/// Returns the installed catalogs, or an empty slice before `set_catalogs`.
#[must_use]
pub fn catalogs() -> &'static [&'static [EventDef]] {
    CATALOGS.lock(|cell| *cell.borrow())
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
