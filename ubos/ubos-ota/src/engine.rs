//! Board-agnostic OTA orchestration.
//!
//! [`OtaEngine`] implements [`ubos_api::OtaTarget`] for any board by driving
//! an [`OtaBackend`]. The engine owns the parts that are identical on every
//! board: tracking the write offset across an upload, the order in which the
//! breadcrumb phases are recorded, and the core-1 park lifecycle. The backend
//! supplies the board-specific primitives: the flash swap mechanism, the
//! breadcrumb store, the reboot, and the per-poll delay used while waiting for
//! the core-1 park acknowledgement.
//!
//! Splitting the logic this way keeps the subtle sequencing — park before any
//! flash access, breadcrumb `IN_FLASH` before each write and `FLASH_DONE`
//! after, release the park on every exit path — in one place so a second
//! board cannot diverge from it.

use ubos_api::{OtaError, OtaWritePhase};

/// Maximum number of park-ack polls before giving up and returning an error.
///
/// 500 polls at the backend's per-poll delay (tuned to about 100 µs) is a
/// 50 ms ceiling. Core 1 is responsive at session start so the ack arrives
/// well within the first millisecond; the budget is purely a safety net
/// against a stalled core. It must stay far under the watchdog timeout
/// because the poll loop is a synchronous busy-spin on core 0 that starves
/// the watchdog feeder while it runs.
const PARK_POLL_MAX_ATTEMPTS: u32 = 500;

/// Board-specific primitives the [`OtaEngine`] drives.
///
/// Every method is synchronous and infallible-to-call so the engine can use
/// it from the object-safe [`ubos_api::OtaTarget`] surface without `await`.
/// The flash-touching methods ([`prepare`](Self::prepare),
/// [`write`](Self::write), [`activate`](Self::activate)) run only while core 1
/// is parked, which the engine guarantees.
///
/// `Send` is required because [`ubos_api::OtaTarget`] is `Send`, and the engine
/// is registered as a `'static` target shared across tasks.
pub trait OtaBackend: Send {
    /// Verifies the running image is in a state that can accept an update and
    /// arms the staging slot. Called by [`OtaEngine`] once per session before
    /// core 1 is parked, so any flash access here must be safe without the
    /// engine's park: a read, or a backend whose flash driver parks core 1
    /// itself.
    ///
    /// # Errors
    ///
    /// Returns an [`OtaError`] if the running image is not in a confirmable
    /// state or the staging slot cannot be armed.
    fn prepare(&mut self) -> Result<(), OtaError>;

    /// Writes one chunk of image bytes at `offset` into the staging slot.
    ///
    /// # Errors
    ///
    /// Returns [`OtaError::SlotOverflow`] if the write runs past the slot, or
    /// [`OtaError::Flash`] on any other flash failure.
    fn write(&mut self, offset: usize, data: &[u8]) -> Result<(), OtaError>;

    /// Commits the staged image so the next boot runs it.
    ///
    /// # Errors
    ///
    /// Returns [`OtaError`] if committing the staged image fails.
    fn activate(&mut self) -> Result<(), OtaError>;

    /// Resets the MCU into the activated image. Never returns. The engine
    /// releases the core-1 park before calling this.
    fn reboot(&mut self) -> !;

    /// Label of the slot the running image booted from.
    fn running_slot_label(&self) -> &'static str;

    /// Whether a revertable previous image exists.
    fn has_previous_image(&self) -> bool;

    /// Version of the previous image, if the backend records one.
    fn previous_version(&self) -> Option<u32>;

    /// Records the OTA breadcrumb at `offset` with `phase`. Backends without
    /// reset-surviving storage may make this a no-op.
    fn write_breadcrumb(&self, offset: u32, phase: OtaWritePhase);

    /// Clears the OTA breadcrumb so a clean reboot is not misread as a
    /// mid-upload reset at the next boot.
    fn clear_breadcrumb(&self);

    /// Spins for roughly one park-poll interval. Called between park-ack
    /// polls; the implementation picks a delay appropriate to its clock.
    fn park_poll_delay(&self);
}

/// Board-agnostic OTA target: an [`OtaBackend`] plus the running version and
/// the live upload offset.
pub struct OtaEngine<B> {
    backend: B,
    firmware_version: u32,
    /// Maximum firmware image the board accepts, reported through
    /// [`ubos_api::OtaTarget::max_image_bytes`]. Set by the board from its
    /// `config.rs` so the accepted upload matches the board's active slot.
    max_image_bytes: u64,
    /// Next write offset into the staging slot, advanced by each
    /// [`write_chunk`](ubos_api::OtaTarget::write_chunk). `None` until
    /// [`begin`](ubos_api::OtaTarget::begin) arms the writer.
    write_offset: Option<usize>,
}

impl<B: OtaBackend> OtaEngine<B> {
    /// Creates an engine over `backend`, reporting `firmware_version` as the
    /// running image version and `max_image_bytes` as the largest upload the
    /// board accepts. No session is active until `begin`.
    #[must_use]
    pub const fn new(backend: B, firmware_version: u32, max_image_bytes: u64) -> Self {
        Self {
            backend,
            firmware_version,
            max_image_bytes,
            write_offset: None,
        }
    }

    /// Requests the core-1 park and spin-waits until the mining worker
    /// acknowledges, bounded by [`PARK_POLL_MAX_ATTEMPTS`].
    ///
    /// Returns `true` once core 1 is parked, or `false` on timeout. The park
    /// is re-requested on every poll iteration: a single cross-core wake can
    /// be missed when core 1's executor is idle in `wfe` and the wake lands
    /// during a brief interrupts-disabled window, so re-signalling each
    /// iteration guarantees the worker is woken within the budget. The caller
    /// releases the park; this only acquires and waits.
    fn park_core1_and_wait(&self) -> bool {
        let mut attempts = 0u32;
        loop {
            ubos_mining::request_core1_park();
            if ubos_mining::core1_is_parked() {
                return true;
            }
            if attempts >= PARK_POLL_MAX_ATTEMPTS {
                return false;
            }
            self.backend.park_poll_delay();
            attempts += 1;
        }
    }
}

impl<B: OtaBackend> ubos_api::OtaTarget for OtaEngine<B> {
    fn bytes_written(&self) -> Option<usize> {
        self.write_offset
    }

    fn running_version(&self) -> u32 {
        self.firmware_version
    }

    fn max_image_bytes(&self) -> u64 {
        self.max_image_bytes
    }

    fn running_slot_label(&self) -> &'static str {
        self.backend.running_slot_label()
    }

    fn has_previous_image(&self) -> bool {
        self.backend.has_previous_image()
    }

    fn previous_version(&self) -> Option<u32> {
        self.backend.previous_version()
    }

    /// Verifies the running image, parks core 1, and arms the writer.
    ///
    /// The backend's `prepare` runs first so a bad bootloader state fails
    /// before core 1 is parked. Core 1 is then parked and held across the
    /// whole upload so the backend's flash writes never race the mining
    /// serial worker; the breadcrumb is armed at offset 0 once the park is
    /// confirmed. On park timeout the park is released and the session fails
    /// closed.
    fn begin(&mut self) -> Result<(), OtaError> {
        self.backend.prepare()?;

        if !self.park_core1_and_wait() {
            defmt::warn!("OTA begin: core 1 did not park within timeout; aborting");
            ubos_mining::release_core1_park();
            // No dedicated timeout variant exists; a failed park means no write
            // can proceed, so it surfaces as a flash failure like any other
            // pre-write abort.
            return Err(OtaError::Flash);
        }

        self.write_offset = Some(0);
        self.backend.write_breadcrumb(0, OtaWritePhase::None);
        Ok(())
    }

    /// Writes the next chunk into the staging slot, bracketing the backend
    /// write with `IN_FLASH`/`FLASH_DONE` breadcrumbs so a reset inside the
    /// write is distinguishable from one between writes.
    fn write_chunk(&mut self, data: &[u8]) -> Result<(), OtaError> {
        let Some(offset) = self.write_offset else {
            return Err(OtaError::Flash);
        };
        // Set IN_FLASH before the write so a reset inside it is identifiable.
        #[allow(clippy::cast_possible_truncation)]
        self.backend
            .write_breadcrumb(offset as u32, OtaWritePhase::InFlash);
        self.backend.write(offset, data)?;
        let new_offset = offset + data.len();
        self.write_offset = Some(new_offset);
        // Set FLASH_DONE after the write returns and record the new offset.
        #[allow(clippy::cast_possible_truncation)]
        self.backend
            .write_breadcrumb(new_offset as u32, OtaWritePhase::FlashDone);
        Ok(())
    }

    /// Commits the staged image, clears the breadcrumb, and releases the park.
    ///
    /// Fails if no session is active. The breadcrumb is cleared and the park
    /// released regardless of the backend result: on success the reboot that
    /// follows must not be misread as a mid-upload reset; on failure the board
    /// does not reboot so no diagnostic is lost, and the park release is safe
    /// because the subsequent `abort` is idempotent.
    fn finish(&mut self) -> Result<(), OtaError> {
        if self.write_offset.is_none() {
            ubos_mining::release_core1_park();
            return Err(OtaError::Flash);
        }
        let result = self.backend.activate();
        self.write_offset = None;
        self.backend.clear_breadcrumb();
        ubos_mining::release_core1_park();
        result
    }

    /// Releases the core-1 park and clears the breadcrumb without committing.
    ///
    /// Called by the REST handler on every error or abandoned-upload path
    /// after `begin` succeeded. Idempotent.
    fn abort(&mut self) {
        ubos_mining::release_core1_park();
        self.backend.clear_breadcrumb();
        self.write_offset = None;
    }

    /// Releases the park and resets into the activated image. Never returns.
    fn reboot_into_new(&mut self) -> ! {
        ubos_mining::release_core1_park();
        self.backend.reboot()
    }
}
