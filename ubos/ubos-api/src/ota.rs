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

//! Board-agnostic OTA update trait and error type.
//!
//! `OtaTarget` abstracts over the flash partition layout and write
//! mechanics so the REST handler in `rest.rs` can drive a firmware
//! update without depending on any board-specific crate. The rp2350,
//! rp2040, and bmm-pico firmware implement this trait; boards that do
//! not supply an implementation receive a 501 Not Implemented response
//! from the handler.
//!
//! All methods are synchronous. The streaming handler sleeps briefly
//! (`Timer::after`) between sector writes so the network task can drain
//! the chip buffer and feed the watchdog; the chunk-ack handler instead
//! writes one sector per request. Implementors that need to pause a
//! second core for the duration of a flash write must do so inside
//! `write_chunk`.
//!
//! # Registration
//!
//! The HTTP `Service` is shared (`&self`), so the target cannot be a
//! `&mut` field. The firmware registers a `&'static mut` implementation
//! once at boot via [`register_ota_target`]; the OTA handlers acquire it
//! through an async [`Mutex`], which also serialises concurrent uploads:
//! a second upload finds the lock held and is rejected. Boards that
//! never register a target leave the endpoint reporting "not available".

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_sync::watch::Watch;
use portable_atomic::{AtomicU8, AtomicU32, Ordering};

use crate::schema::system_info::OtaInfo;

// ---------------------------------------------------------------------------
// Live upload session state (lock-free atomics)
// ---------------------------------------------------------------------------

/// Phase of the current or most recent OTA upload session.
///
/// Stored as a `u8` in an `AtomicU8`. The `#[repr(u8)]` guarantee means
/// any valid discriminant can be loaded atomically and cast back without
/// risk of reading an indeterminate intermediate value.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum OtaPhase {
    /// No upload is in progress. The board is running normally.
    Idle = 0,
    /// An upload session has been opened with `begin()` and chunks are
    /// being received.
    Uploading = 1,
    /// The last upload session ended with an error. `lastError` holds the
    /// mapped error code.
    Error = 2,
    /// `finish()` succeeded and the firmware is about to call
    /// `reboot_into_new()`. The board will reset momentarily.
    Rebooting = 3,
}

impl OtaPhase {
    /// The phase a stored discriminant names; anything unknown reads as
    /// idle, which is the default a fresh boot starts in.
    const fn from_u8(raw: u8) -> Self {
        match raw {
            raw if raw == Self::Uploading as u8 => Self::Uploading,
            raw if raw == Self::Error as u8 => Self::Error,
            raw if raw == Self::Rebooting as u8 => Self::Rebooting,
            _ => Self::Idle,
        }
    }
}

/// Error code mapping for [`OtaError`] variants.
///
/// Zero is reserved for "no error". Each nonzero code identifies one
/// `OtaError` variant so HTTP clients can distinguish flash failures
/// from slot overflows without parsing a string.
#[derive(Clone, Copy, Debug)]
pub struct OtaErrorCode(u8);

impl OtaErrorCode {
    /// Maps an [`OtaError`] to a nonzero `u8` code.
    #[must_use]
    pub fn from_error(error: OtaError) -> Self {
        let code = match error {
            OtaError::NoPartitionTable => 1,
            OtaError::SlotOverflow => 2,
            OtaError::Flash => 3,
            OtaError::ShortUpload => 4,
        };
        Self(code)
    }

    /// Returns the raw `u8` value.
    #[must_use]
    pub fn value(self) -> u8 {
        self.0
    }
}

/// Snapshot of the live upload session state.
///
/// Returned by [`OtaSession::snapshot`] for the `/api/info` response
/// writer. All fields are derived from atomics and are consistent only
/// on a per-field basis; no cross-field ordering is guaranteed.
#[derive(Clone, Copy)]
pub(crate) struct OtaSessionSnapshot {
    /// Current upload phase.
    pub phase: OtaPhase,
    /// Bytes written to the inactive slot in the current session.
    pub bytes_written: u32,
    /// Number of chunks written in the current session.
    pub chunks: u32,
    /// Mapped error code of the last failure, or 0 when none.
    pub last_error: u8,
}

/// Live state of the current or most recent OTA upload session.
///
/// The four fields are independent atomics rather than one lock: the
/// info endpoint reads them without ever touching the upload mutex, so an
/// upload in progress never blocks a status read. Each method updates the
/// fields its caller's transition requires and no others, matching the
/// state machine described on [`OtaPhase`].
pub(crate) struct OtaSession {
    /// Phase of the OTA upload, readable without holding the upload mutex.
    phase: AtomicU8,
    /// Number of bytes written in the current or last upload session.
    bytes_written: AtomicU32,
    /// Number of chunks written in the current or last upload session.
    chunk_count: AtomicU32,
    /// Error code from the last failed upload session, or 0 when none.
    last_error: AtomicU8,
}

/// The single upload session state, reached through [`ota_session`].
static OTA_SESSION: OtaSession = OtaSession {
    phase: AtomicU8::new(OtaPhase::Idle as u8),
    bytes_written: AtomicU32::new(0),
    chunk_count: AtomicU32::new(0),
    last_error: AtomicU8::new(0),
};

/// Returns the live OTA upload session state. A free facade over the
/// private static, so the session has one home and every caller reaches
/// it the same way.
pub(crate) fn ota_session() -> &'static OtaSession {
    &OTA_SESSION
}

impl OtaSession {
    /// Returns a snapshot of the live upload session state.
    ///
    /// Lock-free: never touches the upload mutex, so it stays readable
    /// while an upload holds the target.
    pub(crate) fn snapshot(&self) -> OtaSessionSnapshot {
        // Each field is loaded with Relaxed ordering. This is sufficient
        // for diagnostic reads: the values are eventually consistent and
        // the reader, the info endpoint, only needs a plausible current
        // picture, not a strictly synchronized one.
        let phase = OtaPhase::from_u8(self.phase.load(Ordering::Relaxed));
        OtaSessionSnapshot {
            phase,
            bytes_written: self.bytes_written.load(Ordering::Relaxed),
            chunks: self.chunk_count.load(Ordering::Relaxed),
            last_error: self.last_error.load(Ordering::Relaxed),
        }
    }

    /// Called by the handler when a new upload session begins (offset == 0,
    /// `begin()` succeeded).
    ///
    /// Resets bytes and chunk count to zero, clears the error code, and
    /// sets the phase to `Uploading`.
    pub(crate) fn begin(&self) {
        self.bytes_written.store(0, Ordering::Relaxed);
        self.chunk_count.store(0, Ordering::Relaxed);
        self.last_error.store(0, Ordering::Relaxed);
        self.phase
            .store(OtaPhase::Uploading as u8, Ordering::Relaxed);
    }

    /// Called by the handler after each successful `write_chunk` call.
    ///
    /// Sets the absolute byte count to `total_bytes_written` (not a delta)
    /// and increments the chunk counter by one.
    pub(crate) fn chunk_written(&self, total_bytes_written: u32) {
        self.bytes_written
            .store(total_bytes_written, Ordering::Relaxed);
        self.chunk_count.fetch_add(1, Ordering::Relaxed);
    }

    /// Records an upload error on any error-return path after a session
    /// has started.
    ///
    /// Sets the phase to `Error` and records the mapped error code. Bytes
    /// and chunk count are left unchanged so diagnostics reflect progress
    /// at the time of failure.
    pub(crate) fn record_error(&self, error: OtaError) {
        self.last_error
            .store(OtaErrorCode::from_error(error).value(), Ordering::Relaxed);
        self.phase.store(OtaPhase::Error as u8, Ordering::Relaxed);
    }

    /// Records the error and aborts the target's in-progress write.
    ///
    /// This is the block every handler runs when a write, a read, or
    /// `finish()` fails partway through a session: the error is recorded
    /// first so a client that polls `/api/info` mid-abort sees the cause,
    /// then the target is told to discard the write it cannot complete.
    pub(crate) fn fail(&self, ota: &mut dyn OtaTarget, error: OtaError) {
        self.record_error(error);
        ota.abort();
    }

    /// Called by the handler just before `reboot_into_new()`.
    ///
    /// Sets the phase to `Rebooting`. The board will reset before any
    /// further state changes are possible, so this is the terminal state.
    pub(crate) fn rebooting(&self) {
        self.phase
            .store(OtaPhase::Rebooting as u8, Ordering::Relaxed);
    }

    /// Called by the session watcher when an upload made no progress for
    /// the idle window, after the target has been aborted and mining
    /// resumed.
    ///
    /// Resets the phase to `Idle` so the upload state machine is back to
    /// normal; bytes and chunk count are left unchanged for post-mortem
    /// diagnostics via the info endpoint.
    pub(crate) fn timeout(&self) {
        self.phase.store(OtaPhase::Idle as u8, Ordering::Relaxed);
    }
}

// ---------------------------------------------------------------------------
// Reset diagnostics (published once at boot)
// ---------------------------------------------------------------------------

/// Cause of the most recent reset, as reported by the watchdog hardware.
///
/// Published once by the firmware at boot via [`publish_reset_diag`] and
/// read lock-free by the info endpoints via [`reset_diag`].
///
/// A `Clean` reset means the watchdog reason register was clear on boot:
/// either a power-on reset or a deliberate `SCB::sys_reset()` (which
/// does not set the watchdog reason bits on RP2350). Only a hardware
/// watchdog timeout sets the `TimedOut` bit.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum ResetReason {
    /// Watchdog reason register was not checked (pre-publication state).
    Unknown = 0,
    /// Watchdog reason bits were both clear: power-on reset or `sys_reset`.
    Clean = 1,
    /// The watchdog timer reached zero before the next feed.
    WatchdogTimeout = 2,
    /// The watchdog force bit was set (software-triggered watchdog reset).
    WatchdogForced = 3,
}

/// Write phase recorded in the watchdog SCRATCH2 register at the time of reset.
///
/// Stored as a `u8` discriminant in SCRATCH2 so the next boot can determine
/// exactly where in the OTA write sequence the board was when it reset.
/// Distinguishes a slow-flash watchdog timeout (the flash op itself ran long)
/// from a panic (the panic handler ran before the reset fired).
///
/// Discriminant values must match the `WRITE_PHASE_*` constants in
/// `ubos/ubos-ota/src/rp.rs`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[repr(u8)]
pub enum OtaWritePhase {
    /// No OTA write was in progress at the time of reset.
    None = 0,
    /// Reset occurred inside the embassy-boot flash write call; the write had
    /// not returned when the watchdog fired.
    InFlash = 1,
    /// Reset occurred after the flash write returned successfully; the offset
    /// was updated and the breadcrumb was written before the board reset.
    FlashDone = 2,
    /// A panic handler ran before the reset; the panic breadcrumb was written
    /// to SCRATCH2 by `ota_target::record_panic_breadcrumb`.
    Panic = 3,
}

impl OtaWritePhase {
    /// Maps a raw register value to the corresponding [`OtaWritePhase`].
    ///
    /// Unknown values map to [`OtaWritePhase::None`] so stale or
    /// uninitialized SCRATCH2 contents are treated conservatively.
    #[must_use]
    pub fn from_u32(value: u32) -> Self {
        match value {
            1 => Self::InFlash,
            2 => Self::FlashDone,
            3 => Self::Panic,
            _ => Self::None,
        }
    }
}

/// Published reset diagnostics snapshot.
///
/// Captured at boot from the watchdog reason register and the OTA
/// breadcrumb in the watchdog scratch registers. Published once via
/// [`publish_reset_diag`]; zero-value until published.
#[derive(Clone, Copy)]
pub struct ResetDiag {
    /// Cause of the most recent reset.
    pub reason: ResetReason,
    /// Whether an OTA upload session was active when the reset occurred,
    /// as indicated by the breadcrumb written to the watchdog scratch
    /// registers during the upload.
    pub ota_was_active: bool,
    /// The byte offset reached in the OTA slot at the time of reset, as
    /// recorded in the breadcrumb. Zero when `ota_was_active` is `false`.
    pub ota_offset: u32,
    /// Write phase recorded in SCRATCH2 at the time of reset.
    ///
    /// Indicates whether the reset happened inside a flash write, after
    /// it returned, or was preceded by a panic. `None` means no OTA
    /// write phase was recorded (SCRATCH2 was zero or unset).
    pub write_phase: OtaWritePhase,
}

/// Reset diagnostics, published once at boot; zero until published.
static RESET_DIAG: Watch<CriticalSectionRawMutex, ResetDiag, 1> = Watch::new();

/// Publishes the reset diagnostics captured at boot.
///
/// Call once, early in the boot sequence, after reading the watchdog
/// reason register and the OTA breadcrumb from the scratch registers.
/// Subsequent calls are no-ops because `Watch::sender().send()` replaces
/// the stored value, but the fields represent a one-time boot-time
/// capture and should only be written once.
pub fn publish_reset_diag(
    reason: ResetReason,
    ota_was_active: bool,
    ota_offset: u32,
    write_phase: OtaWritePhase,
) {
    RESET_DIAG.sender().send(ResetDiag {
        reason,
        ota_was_active,
        ota_offset,
        write_phase,
    });
}

/// Returns the published reset diagnostics, or `None` before publication.
///
/// Lock-free: reads the `Watch` snapshot without blocking.
pub(crate) fn reset_diag() -> Option<ResetDiag> {
    RESET_DIAG.try_get()
}

/// Latest OTA info snapshot, published once when the board registers its
/// target and read lock-free by the info endpoints via [`ota_info`].
///
/// The fields are board-fixed — a running image's version and slot do
/// not change while it runs — so a published-once `Watch` is the right
/// shape: readers take a relaxed snapshot with [`Watch::try_get`] rather
/// than locking the upload [`Mutex`] just to report constants. `None`
/// until published, and `None` on boards that never register a target.
static OTA_INFO: Watch<CriticalSectionRawMutex, OtaInfo, 1> = Watch::new();

/// The most bytes of a build id the info bodies carry. The build
/// environment can hand the id whole and bounds nothing, so the bound
/// stands here where the body buffers are sized. How a longer id
/// is cut is [`truncate_build_id`]'s to say. A packer outside this tree
/// holds its own copy of this number, and a check in that repo compares
/// the two. That check reads this constant through its pinned copy of
/// this tree, so raising the bound here leaves both repositories green
/// until that pin moves, and the mismatch surfaces then rather than on
/// the branch that caused it.
pub const MAX_BUILD_ID_LEN: usize = 48;

/// `build` cut to at most [`MAX_BUILD_ID_LEN`] bytes, at the last
/// character boundary at or below it. Nothing holds the id to ASCII, and
/// the bound is what the info bodies are sized for, so the cut lands short
/// of it rather than past it.
pub(crate) fn truncate_build_id(build: &'static str) -> &'static str {
    let mut end = MAX_BUILD_ID_LEN.min(build.len());
    while !build.is_char_boundary(end) {
        end -= 1;
    }
    &build[..end]
}

/// Latest published OTA info, or `None` on boards without an OTA target.
///
/// Lock-free: never touches the upload mutex, so it stays readable even
/// while an upload holds the target.
pub(crate) fn ota_info() -> Option<OtaInfo> {
    OTA_INFO.try_get()
}

/// The registered OTA target, or `None` until the firmware registers one.
///
/// Why an async `Mutex` in a `static`, rather than a plain field or a
/// `RefCell`:
/// - `edge_http`'s `Service::handle` takes `&self` (the service is shared
///   across connections), so the target cannot be reached as a `&mut`
///   field; it needs interior mutability.
/// - The streaming upload holds one mutable borrow of the target across
///   every `await` in the read loop, so the guard must survive `.await`
///   — that rules out `RefCell`/blocking mutexes and requires an async
///   [`Mutex`].
/// - A `static` must be `Sync`; the only async-mutex flavour that is `Sync`
///   is one backed by a cross-context `RawMutex`, hence
///   [`CriticalSectionRawMutex`] (a `NoopRawMutex` mutex is not `Sync`).
/// - `try_lock` then gives upload serialisation for free: a second upload
///   finds the lock held and is rejected rather than racing the flash.
static OTA_TARGET: Mutex<CriticalSectionRawMutex, Option<&'static mut dyn OtaTarget>> =
    Mutex::new(None);

/// Registers the board's OTA implementation. Call once at boot.
///
/// Takes a `&'static mut` because the implementation owns flash work
/// areas that must outlive every request and be mutated during a write.
pub async fn register_ota_target(target: &'static mut dyn OtaTarget) {
    // Publish the board-fixed info snapshot so the info endpoints can read
    // it lock-free, then store the target for the upload path.
    OTA_INFO.sender().send(OtaInfo {
        running_slot: target.running_slot_label(),
        running_version: target.running_version(),
        has_previous_image: target.has_previous_image(),
        previous_version: target.previous_version(),
        max_image_bytes: target.max_image_bytes(),
    });
    *OTA_TARGET.lock().await = Some(target);
}

/// Accessor for the OTA handlers to acquire the registered target.
///
/// This hands out the mutex rather than the guarded value, which a typed
/// accessor would normally hide. The upload holds one borrow across the
/// whole read loop and rejects a second upload with `try_lock`, so the
/// caller owns both the span and the refusal and there is no method
/// boundary to put them behind. Kept `pub(crate)` so the callers stay in
/// this crate and can be counted: `rest.rs` and `system.rs` are all of
/// them, and each one uses `try_lock`.
pub(crate) fn target_mutex()
-> &'static Mutex<CriticalSectionRawMutex, Option<&'static mut dyn OtaTarget>> {
    &OTA_TARGET
}

/// Error variants for OTA begin / write / finish operations.
///
/// Variants are kept coarse-grained because the handler only needs to
/// distinguish "the device is not set up for OTA" from "a write
/// failed" from "the uploaded image was too short".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum OtaError {
    /// The device has no dual-partition layout; OTA is not possible.
    NoPartitionTable,
    /// The image to be written exceeds the inactive slot capacity.
    SlotOverflow,
    /// The underlying flash driver reported an error.
    Flash,
    /// `finish` was called before enough bytes were written.
    ShortUpload,
}

/// Board-agnostic interface for writing a firmware image to the
/// inactive OTA slot and rebooting into it.
///
/// Implementors manage the dual-partition flash layout and the
/// core-1 pause/resume cycle required for safe concurrent flash access
/// on RP2350. The REST handler never calls flash APIs directly; it
/// drives this trait only.
///
/// All methods are synchronous (`fn`, not `async fn`) so the trait is
/// object-safe and the handler can hold a `dyn OtaTarget` reference
/// across its `async` await points without an extra `Pin`.
///
/// The streaming handler sleeps briefly between `write_chunk` calls so
/// the network task can drain the chip buffer and feed the watchdog.
///
/// `Send` is required because the registered target lives in a `static`
/// behind a [`Mutex`]; the bound is satisfied by any plain-data
/// implementation. The RP2350 target holds only flash buffers.
pub trait OtaTarget: Send {
    /// Number of bytes written since the last [`begin`] call, or `None`
    /// when no session is active (before the first `begin` or after
    /// `finish`/error reset).
    ///
    /// The chunk-ack handler uses this to validate that each incoming
    /// `X-OTA-Offset` matches the actual write position, so out-of-order
    /// or replayed requests are rejected rather than silently corrupting
    /// the staged image.
    ///
    /// [`begin`]: OtaTarget::begin
    fn bytes_written(&self) -> Option<usize>;
    /// Version number embedded in the currently-running firmware image.
    fn running_version(&self) -> u32;

    /// Human-readable label for the currently-running slot.
    ///
    /// Returns a board-defined label such as `"ACTIVE"`.
    fn running_slot_label(&self) -> &'static str;

    /// Returns `true` when a previously-written image exists in the
    /// inactive slot.
    fn has_previous_image(&self) -> bool;

    /// Version number of the image in the inactive slot, or `None`
    /// when no previous image is present.
    fn previous_version(&self) -> Option<u32>;

    /// Maximum firmware image size, in bytes, the board accepts for an
    /// upload. The handlers reject a larger image before any write.
    ///
    /// Defaults to the RP active-slot budget. A board whose inactive slot
    /// has a different capacity overrides this.
    fn max_image_bytes(&self) -> u64 {
        crate::system::OTA_MAX_IMAGE_BYTES
    }

    /// Prepare the inactive slot to receive a new firmware image.
    ///
    /// Arms the slot writer and may perform any target-specific preparation.
    /// Partial-write safety comes from `finish()` not being called, not from
    /// any action taken here.
    ///
    /// # Errors
    ///
    /// Returns [`OtaError::NoPartitionTable`] on boards that have no
    /// dual-partition layout, or [`OtaError::Flash`] on a flash error.
    fn begin(&mut self) -> Result<(), OtaError>;

    /// Write the next chunk of firmware bytes into the inactive slot.
    ///
    /// The implementation is responsible for pausing core 1 around
    /// the flash write and resuming it afterwards. The handler feeds
    /// chunks of 4096 bytes except the final chunk, which
    /// carries the remainder and may be smaller. The target must accept
    /// a sub-sector final write.
    ///
    /// # Errors
    ///
    /// Returns [`OtaError::SlotOverflow`] if the cumulative write would
    /// exceed the inactive slot, or [`OtaError::Flash`] on a flash error.
    fn write_chunk(&mut self, data: &[u8]) -> Result<(), OtaError>;

    /// Validate and commit the written image, finalising the slot header.
    ///
    /// Called once after the last `write_chunk`. Implementations may flush
    /// any write buffer and perform integrity checks at this point.
    ///
    /// # Errors
    ///
    /// Returns [`OtaError::Flash`] on failure. Not calling `finish` after
    /// writing is how a partial update is kept from being booted.
    fn finish(&mut self) -> Result<(), OtaError>;

    /// Reboot the device into the freshly written inactive slot.
    ///
    /// This function never returns. The implementation should include
    /// a short delay so the HTTP 200 acknowledgement has time to reach
    /// the wire before the reset fires.
    fn reboot_into_new(&mut self) -> !;

    /// Release any resources acquired by [`begin`] without committing the
    /// update.
    ///
    /// Called by the handler on every error or abandoned-upload path after
    /// [`begin`] has succeeded, so a failed session leaves no lingering
    /// state (for example, a parked core or a stale OTA breadcrumb).
    /// The default implementation is a no-op; boards that need cleanup
    /// override this method.
    ///
    /// Must be idempotent: calling it when no session is active must be safe.
    ///
    /// [`begin`]: OtaTarget::begin
    fn abort(&mut self) {}
}
