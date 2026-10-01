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
//! [`OtaTarget`] hides the flash layout and the write mechanics, so the
//! REST handlers drive an update without a board crate. A board that
//! registers no target answers 501 from the handler.
//!
//! # Registration
//!
//! The HTTP `Service` is shared, so the target cannot be a `&mut` field.
//! The firmware registers a `&'static mut` target once at boot through
//! [`register_ota_target`], and the handlers take it through an async
//! [`Mutex`]: the streaming upload holds the guard across every `await`
//! in its read loop, which rules out a `RefCell` or a blocking mutex, and
//! `try_lock` rejects a second upload instead of racing the flash. The
//! `CriticalSectionRawMutex` flavour is the one that is `Sync`, which a
//! `static` needs.

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_sync::watch::Watch;
use portable_atomic::{AtomicU8, AtomicU32, Ordering};
use serde::Serialize;
use ubos_http::{Status, StatusMessage};

use crate::schema::system_info::OtaInfo;

/// The config that [`OtaTarget::staged_config`] reports, re-exported so a
/// target names it without depending on `ubos-config`.
pub use ubos_config::StagedConfig;

/// Phase of the current or most recent OTA upload session. Stored as a
/// `u8` in an `AtomicU8`.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(
    feature = "schema",
    schemars(description = "Phase of the current or most recent firmware upload.")
)]
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[repr(u8)]
pub enum OtaPhase {
    /// No upload is in progress. The board is running normally.
    Idle = 0,
    /// An upload session is open and chunks are being received.
    Uploading = 1,
    /// The last upload session ended with an error. `lastError` holds the
    /// mapped error code.
    Error = 2,
    /// The upload finished and the firmware is about to reboot into the
    /// new image.
    Rebooting = 3,
}

impl OtaPhase {
    /// Anything unknown reads as idle, the state a fresh boot starts in.
    const fn from_u8(raw: u8) -> Self {
        match raw {
            raw if raw == Self::Uploading as u8 => Self::Uploading,
            raw if raw == Self::Error as u8 => Self::Error,
            raw if raw == Self::Rebooting as u8 => Self::Rebooting,
            _ => Self::Idle,
        }
    }
}

/// Snapshot of the live upload session state for the `/api/info` writer.
/// Each field is read from its own atomic, so there is no cross-field
/// ordering.
#[derive(Clone, Copy)]
pub(crate) struct OtaSessionSnapshot {
    pub phase: OtaPhase,
    pub bytes_written: u32,
    pub chunks: u32,
    /// Mapped error code of the last failure, or 0 when none.
    pub last_error: u8,
}

/// Live state of the current or most recent OTA upload session. Four
/// independent atomics rather than one lock, so the info endpoint never
/// touches the upload mutex and an upload never blocks a status read.
pub(crate) struct OtaSession {
    phase: AtomicU8,
    bytes_written: AtomicU32,
    chunk_count: AtomicU32,
    /// Error code from the last failed upload session, or 0 when none.
    last_error: AtomicU8,
}

static OTA_SESSION: OtaSession = OtaSession {
    phase: AtomicU8::new(OtaPhase::Idle as u8),
    bytes_written: AtomicU32::new(0),
    chunk_count: AtomicU32::new(0),
    last_error: AtomicU8::new(0),
};

/// The live OTA upload session state; a facade over the private static.
pub(crate) fn ota_session() -> &'static OtaSession {
    &OTA_SESSION
}

impl OtaSession {
    /// Relaxed loads: the info endpoint needs a plausible picture, not a
    /// synchronized one.
    pub(crate) fn snapshot(&self) -> OtaSessionSnapshot {
        let phase = OtaPhase::from_u8(self.phase.load(Ordering::Relaxed));
        OtaSessionSnapshot {
            phase,
            bytes_written: self.bytes_written.load(Ordering::Relaxed),
            chunks: self.chunk_count.load(Ordering::Relaxed),
            last_error: self.last_error.load(Ordering::Relaxed),
        }
    }

    pub(crate) fn begin(&self) {
        self.bytes_written.store(0, Ordering::Relaxed);
        self.chunk_count.store(0, Ordering::Relaxed);
        self.last_error.store(0, Ordering::Relaxed);
        self.phase
            .store(OtaPhase::Uploading as u8, Ordering::Relaxed);
    }

    /// `total_bytes_written` is absolute, not a delta.
    pub(crate) fn chunk_written(&self, total_bytes_written: u32) {
        self.bytes_written
            .store(total_bytes_written, Ordering::Relaxed);
        self.chunk_count.fetch_add(1, Ordering::Relaxed);
    }

    /// Bytes and chunk count are left as they were, so diagnostics show
    /// the progress at the time of failure.
    pub(crate) fn record_error(&self, error: OtaError) {
        self.last_error.store(error as u8, Ordering::Relaxed);
        self.phase.store(OtaPhase::Error as u8, Ordering::Relaxed);
    }

    /// Records the error and aborts the target's in-progress write, in
    /// that order, so a client polling `/api/info` mid-abort sees the
    /// cause.
    pub(crate) fn fail(&self, ota: &mut dyn OtaTarget, error: OtaError) {
        self.record_error(error);
        ota.abort();
    }

    pub(crate) fn rebooting(&self) {
        self.phase
            .store(OtaPhase::Rebooting as u8, Ordering::Relaxed);
    }

    /// Called by the session watcher once an idle upload has been aborted.
    /// Bytes and chunk count stay for the post-mortem.
    pub(crate) fn timeout(&self) {
        self.phase.store(OtaPhase::Idle as u8, Ordering::Relaxed);
    }
}

/// Cause of the most recent reset, from the watchdog reason register.
/// `Clean` covers a power-on reset and a deliberate `sys_reset`, which
/// does not set the reason bits.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(
    feature = "schema",
    schemars(
        description = "Cause of the most recent reset, as the watchdog hardware reported it."
    )
)]
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[repr(u8)]
pub enum ResetReason {
    /// The reset reason was not read before this report.
    Unknown = 0,
    /// Watchdog reason bits were both clear: power-on reset or `sys_reset`.
    Clean = 1,
    /// The watchdog timer reached zero before the next feed.
    WatchdogTimeout = 2,
    /// The watchdog force bit was set (software-triggered watchdog reset).
    WatchdogForced = 3,
}

/// Where in the OTA write sequence the board was when it reset, read
/// back from the watchdog SCRATCH2 register. It tells a flash write that
/// ran long from a panic. The discriminants must match the
/// `WRITE_PHASE_*` constants in `ubos/ubos-ota/src/rp.rs`.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(
    feature = "schema",
    schemars(description = "Where in a flash write the most recent reset landed.")
)]
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[serde(rename_all = "camelCase")]
#[repr(u8)]
pub enum OtaWritePhase {
    /// No OTA write was in progress at the time of reset.
    None = 0,
    /// Reset occurred inside the flash write; the write had not returned
    /// when the watchdog fired.
    InFlash = 1,
    /// Reset occurred after the flash write returned successfully; the offset
    /// was updated and the breadcrumb was written before the board reset.
    FlashDone = 2,
    /// A panic handler ran before the reset and recorded where the write
    /// stood.
    Panic = 3,
}

impl OtaWritePhase {
    /// Unknown values map to `None`, so stale SCRATCH2 contents read as
    /// no write in progress.
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

/// Reset diagnostics captured at boot from the watchdog reason register
/// and the OTA breadcrumb in the scratch registers.
#[derive(Clone, Copy)]
pub struct ResetDiag {
    pub reason: ResetReason,
    /// An OTA upload was active when the board reset.
    pub ota_was_active: bool,
    /// The byte offset reached in the OTA slot; zero when no upload was
    /// active.
    pub ota_offset: u32,
    pub write_phase: OtaWritePhase,
}

static RESET_DIAG: Watch<CriticalSectionRawMutex, ResetDiag, 1> = Watch::new();

/// Publishes the reset diagnostics. Called once, early in boot.
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

/// The published reset diagnostics, or `None` before publication.
pub(crate) fn reset_diag() -> Option<ResetDiag> {
    RESET_DIAG.try_get()
}

/// OTA info published once at target registration. The fields do not
/// change while an image runs, so the info endpoints read a `Watch`
/// instead of locking the upload [`Mutex`] to report constants.
static OTA_INFO: Watch<CriticalSectionRawMutex, OtaInfo, 1> = Watch::new();

/// The most bytes of a build id the info bodies carry; the build
/// environment can hand the id whole and bounds nothing. A packer
/// outside this tree holds its own copy of this number and gates it
/// against this one through its pin, so a change here surfaces there
/// when the pin moves.
pub const MAX_BUILD_ID_LEN: usize = 48;

/// `build` cut to at most [`MAX_BUILD_ID_LEN`] bytes, at the last
/// character boundary at or below it, since nothing holds the id to
/// ASCII.
pub(crate) fn truncate_build_id(build: &'static str) -> &'static str {
    let mut end = MAX_BUILD_ID_LEN.min(build.len());
    while !build.is_char_boundary(end) {
        end -= 1;
    }
    &build[..end]
}

/// The published OTA info, or `None` on boards without an OTA target.
pub(crate) fn ota_info() -> Option<OtaInfo> {
    OTA_INFO.try_get()
}

/// The registered OTA target, or `None` until the firmware registers one.
/// See the module doc for the mutex choice.
static OTA_TARGET: Mutex<CriticalSectionRawMutex, Option<&'static mut dyn OtaTarget>> =
    Mutex::new(None);

/// Registers the board's OTA implementation. Called once at boot.
pub async fn register_ota_target(target: &'static mut dyn OtaTarget) {
    OTA_INFO.sender().send(OtaInfo {
        running_slot: target.running_slot_label(),
        running_version: target.running_version(),
        has_previous_image: target.has_previous_image(),
        previous_version: target.previous_version(),
        max_image_bytes: target.max_image_bytes(),
    });
    *OTA_TARGET.lock().await = Some(target);
}

/// The target's mutex itself, not the guarded value: the upload owns both
/// the locked span and the `try_lock` refusal, so there is no method
/// boundary to put them behind. `pub(crate)` keeps the callers countable.
pub(crate) fn target_mutex()
-> &'static Mutex<CriticalSectionRawMutex, Option<&'static mut dyn OtaTarget>> {
    &OTA_TARGET
}

/// Error variants for OTA begin / write / finish operations.
///
/// Variants are kept coarse-grained because the handler only needs to
/// distinguish "the device is not set up for OTA" from "a write
/// failed" from "the uploaded image was too short" from "the image is
/// for other hardware" from "the connection failed" from "the file is not
/// an image" from "a chunk broke the order of the upload".
///
/// The discriminant of each variant is its code, which `/api/info` reports
/// as `lastError`, so a client tells the causes apart without parsing a
/// string. Zero stays free for no error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[repr(u8)]
pub enum OtaError {
    /// The device has no dual-partition layout; OTA is not possible.
    NoPartitionTable = 1,
    /// The image to be written exceeds the inactive slot capacity. The
    /// handler answers it as a payload too large, because the client sent a
    /// file that the board cannot hold.
    SlotOverflow = 2,
    /// The board could not write the image. The flash reported an error, or
    /// the target could not start or continue the write, as when core 1
    /// does not park or no session is open. The handler answers it as an
    /// internal error, because the fault is on the board.
    Flash = 3,
    /// `finish` was called before enough bytes were written.
    ShortUpload = 4,
    /// The image is for other hardware than this board. The target
    /// refuses it before it writes anything, and the handler answers it as
    /// a bad request, because the client sent the wrong file.
    WrongHardware = 5,
    /// Reading the upload from the connection failed. The handler records
    /// it for a socket error, so `lastError` names the cause.
    Connection = 6,
    /// The upload is not a valid image: its header is malformed, or its
    /// payload does not match the size or the checksum that the header
    /// declares. The client sent a bad file, so the handler answers it as
    /// a bad request.
    InvalidImage = 7,
    /// The offset of a chunk does not match the bytes already written, as
    /// for a chunk sent out of order or a second time. The handler ends the
    /// session and answers it as a conflict, so the client restarts from
    /// offset 0.
    OffsetMismatch = 8,
    /// A chunk before the last one is shorter than one sector. The targets
    /// write one sector per chunk, so the handler ends the session and
    /// answers it as a bad request.
    ShortChunk = 9,
}

/// The status and the message that the handler answers an OTA error
/// with. Each variant states its own here, so a new variant does not
/// compile until its status is chosen.
impl From<OtaError> for StatusMessage {
    fn from(error: OtaError) -> Self {
        match error {
            OtaError::NoPartitionTable => {
                Status::CONFLICT.with_message("device not provisioned for OTA")
            }
            OtaError::SlotOverflow => {
                Status::PAYLOAD_TOO_LARGE.with_message("The image does not fit in the update slot.")
            }
            OtaError::Flash => {
                Status::INTERNAL_SERVER_ERROR.with_message("The board could not write the image.")
            }
            OtaError::ShortUpload => Status::BAD_REQUEST.with_message("The upload is too short."),
            OtaError::WrongHardware => {
                Status::BAD_REQUEST.with_message("The image is not for this hardware.")
            }
            OtaError::Connection => {
                Status::BAD_REQUEST.with_message("The connection failed during the upload.")
            }
            OtaError::InvalidImage => {
                Status::BAD_REQUEST.with_message("The file is not a valid firmware image.")
            }
            OtaError::OffsetMismatch => Status::CONFLICT
                .with_message("chunk offset does not match write position; restart from offset 0"),
            OtaError::ShortChunk => {
                Status::BAD_REQUEST.with_message("non-final chunk must be exactly one sector")
            }
        }
    }
}

/// Writes a firmware image to the inactive OTA slot and reboots into it.
///
/// Every method is synchronous so the trait is object-safe and the
/// handler holds a `dyn OtaTarget` across its await points. A target
/// that must pause a second core around a flash write does so inside
/// `write_chunk`. `Send` because the target lives in a `static` behind a
/// [`Mutex`].
pub trait OtaTarget: Send {
    /// Bytes written since the last [`begin`], or `None` when no session
    /// is active. The chunk-ack handler checks each `X-OTA-Offset` against
    /// it, so a replayed or out-of-order request is rejected.
    ///
    /// [`begin`]: OtaTarget::begin
    fn bytes_written(&self) -> Option<usize>;
    fn running_version(&self) -> u32;

    /// Board-defined label such as `"ACTIVE"`.
    fn running_slot_label(&self) -> &'static str;

    fn has_previous_image(&self) -> bool;

    /// Version of the image in the inactive slot, or `None` when there is
    /// none.
    fn previous_version(&self) -> Option<u32>;

    /// Largest image the board accepts; the handlers reject a larger one
    /// before any write. Defaults to the RP active-slot budget.
    fn max_image_bytes(&self) -> u64 {
        crate::system::OTA_MAX_IMAGE_BYTES
    }

    /// Prepare the inactive slot for a new image. A partial write stays
    /// unbootable because `finish` is never called, not because of
    /// anything done here.
    ///
    /// # Errors
    ///
    /// [`OtaError::NoPartitionTable`] on a board with no dual-partition
    /// layout, or [`OtaError::Flash`].
    fn begin(&mut self) -> Result<(), OtaError>;

    /// Write the next chunk into the inactive slot. Chunks are 4096 bytes
    /// except the last, which the target must accept short.
    ///
    /// # Errors
    ///
    /// Returns [`OtaError::SlotOverflow`] if the cumulative write would
    /// exceed the inactive slot, [`OtaError::WrongHardware`] if the image
    /// is for other hardware than this board, [`OtaError::InvalidImage`] if
    /// the upload is not a valid image, or [`OtaError::Flash`] on a flash
    /// error.
    fn write_chunk(&mut self, data: &[u8]) -> Result<(), OtaError>;

    /// Validate and commit the written image, finalising the slot header.
    ///
    /// # Errors
    ///
    /// Returns [`OtaError::InvalidImage`] if the payload does not match the
    /// size or the checksum that the image declares, or [`OtaError::Flash`]
    /// on any other failure. Not calling `finish` after writing is how a
    /// partial update is kept from being booted.
    fn finish(&mut self) -> Result<(), OtaError>;

    /// The config that the image [`finish`] accepted reads, or `None`
    /// when the board cannot tell.
    ///
    /// The handler hands it to the config task before the reboot, and
    /// the task stores the record that the image reads. On `None` the
    /// handler downgrades nothing.
    ///
    /// [`finish`]: OtaTarget::finish
    fn staged_config(&self) -> Option<StagedConfig>;

    /// Reboot into the freshly written slot, after a short delay so the
    /// HTTP 200 reaches the wire first.
    fn reboot_into_new(&mut self) -> !;

    /// Release what [`begin`] acquired without committing, on every error
    /// or abandoned-upload path. Must be idempotent.
    ///
    /// [`begin`]: OtaTarget::begin
    fn abort(&mut self) {}
}
