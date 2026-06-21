//! OTA target implementation shared by all RP board firmware crates.
//!
//! [`RpOtaTarget`] implements [`ubos_api::OtaTarget`] on top of
//! `embassy-boot`'s [`BlockingFirmwareUpdater`]. The updater writes the
//! incoming image into the DFU partition and flips the bootloader state
//! partition's magic; the RP bootloader performs the active/DFU swap on the
//! next boot. The target is constructed once at boot, registered with
//! [`ubos_api::register_ota_target`], and then driven exclusively through the
//! REST handler's `dyn OtaTarget` reference.
//!
//! The DFU and STATE partitions are [`BlockingPartition`]s over the one
//! shared `'static` flash mutex (see [`crate::shared_flash`]). Their offsets
//! and sizes come from the `__bootloader_dfu_*` / `__bootloader_state_*`
//! linker symbols defined in `memory.x`, matching the layout the bootloader
//! links against.
//!
//! Core 1 runs the ubos-mining serial worker. Every flash-writing path
//! must park core 1 explicitly first: `embassy-rp`'s `pause_core1()`
//! handshake stalls >15 s when core 1's executor is mid-`AtomicU64` seqlock
//! (the `portable-atomic` fallback disables interrupts, blocking the
//! `SIO_IRQ_FIFO` pause handshake), tripping the watchdog before the write
//! completes. `begin()` pre-parks core 1 and holds the park across the
//! whole upload; the park is released on every exit path.
//!
//! # OTA breadcrumb
//!
//! The watchdog SCRATCH0, SCRATCH1, and SCRATCH2 registers survive a warm
//! reset (watchdog timeout, `SCB::sys_reset`) but are cleared by a power-on
//! reset. SCRATCH0–3 are free for firmware use; the RP bootrom reserves
//! SCRATCH4–7 for its watchdog-boot vectoring, and embassy-boot-rp itself
//! uses no scratch registers, so SCRATCH0–2 are safe to use.
//!
//! `begin()` writes a nonzero magic value into SCRATCH0, 0 into SCRATCH1,
//! and 0 (None phase) into SCRATCH2. In `write_chunk()`, SCRATCH2 is set to
//! `WRITE_PHASE_IN_FLASH` before the flash write and `WRITE_PHASE_FLASH_DONE`
//! after it returns, so a watchdog timeout inside the write is distinguishable
//! from one that happened between writes. `finish()` clears both SCRATCH0 and
//! SCRATCH2 so a successful update does not leave stale state after the
//! intended reboot. `record_panic_breadcrumb()` writes `WRITE_PHASE_PANIC`
//! into SCRATCH2 unconditionally from the panic handler so any panic — even
//! one outside an OTA upload — is recorded and survives the subsequent
//! watchdog reset. If the board resets mid-upload, main.rs reads the magic,
//! offset, and phase at boot and publishes them as reset diagnostics so the
//! cause is visible over HTTP without a debug probe.

use defmt::{info, warn};
use embassy_boot::FirmwareUpdaterError;
use embassy_boot_rp::{BlockingFirmwareUpdater, FirmwareUpdaterConfig, State};
use embassy_embedded_hal::flash::partition::BlockingPartition;
use embassy_rp::flash::{self, Blocking, Flash};
use embassy_rp::pac;
use embassy_rp::peripherals::FLASH;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embedded_storage::nor_flash::ReadNorFlash;
use ubos_api::OtaError as ApiError;

use crate::shared_flash::SharedFlashMutex;

/// Maximum number of park-ack polls before giving up and returning an error.
///
/// 500 × 100 µs = 50 ms. Core 1 is responsive at session start so the ack
/// arrives well within the first millisecond; the budget is purely a
/// safety net against a stalled core.
const PARK_POLL_MAX_ATTEMPTS: u32 = 500;

/// Magic value written to SCRATCH0 when an OTA upload session is active.
///
/// The value is nonzero and distinctive enough to survive accidental
/// coincidence with uninitialized RAM. Cleared to zero by `finish()` so a
/// successful update does not leave the breadcrumb set after the intended
/// reboot. A power-on reset also clears the scratch registers, so stale
/// breadcrumbs from a previous power cycle do not survive a cold boot.
///
/// Published so `main.rs` can compare the boot-time SCRATCH0 value without
/// duplicating the constant.
pub const OTA_BREADCRUMB_MAGIC: u32 = 0x4F54_5541; // ASCII "OTUA"

/// Write-phase code: no OTA write in progress.
///
/// These constants MUST match the discriminants of `ubos_api::OtaWritePhase`.
const WRITE_PHASE_NONE: u32 = 0;

/// Write-phase code: reset happened inside the embassy-boot flash write call.
///
/// These constants MUST match the discriminants of `ubos_api::OtaWritePhase`.
const WRITE_PHASE_IN_FLASH: u32 = 1;

/// Write-phase code: reset happened after the flash write returned.
///
/// These constants MUST match the discriminants of `ubos_api::OtaWritePhase`.
const WRITE_PHASE_FLASH_DONE: u32 = 2;

/// Write-phase code: a panic handler ran before the reset.
///
/// These constants MUST match the discriminants of `ubos_api::OtaWritePhase`.
const WRITE_PHASE_PANIC: u32 = 3;

// Enforce the "MUST match" invariant above at compile time: a divergence
// would silently corrupt the SCRATCH2 diagnostic written on reset.
const _: () = assert!(WRITE_PHASE_NONE == ubos_api::OtaWritePhase::None as u32);
const _: () = assert!(WRITE_PHASE_IN_FLASH == ubos_api::OtaWritePhase::InFlash as u32);
const _: () = assert!(WRITE_PHASE_FLASH_DONE == ubos_api::OtaWritePhase::FlashDone as u32);
const _: () = assert!(WRITE_PHASE_PANIC == ubos_api::OtaWritePhase::Panic as u32);

// `write_chunk` builds a fresh `BlockingFirmwareUpdater` per call, which
// erases each DFU sector once before its first write. Correctness requires
// every chunk to cover exactly one erase sector, so the API's chunk size
// must equal the flash erase size. A smaller chunk would land the next
// write mid-sector and a fresh updater would re-erase it, dropping the
// bytes already written. The runtime `debug_assert_eq!` in `write_chunk`
// catches the resulting misaligned offset in debug builds; this guards the
// coupling at compile time in every build.
const _: () = assert!(ubos_api::system::OTA_CHUNK_SIZE == flash::ERASE_SIZE);

/// Writes the OTA breadcrumb into the watchdog scratch registers.
///
/// SCRATCH0 receives the magic, SCRATCH1 receives `offset`, and SCRATCH2
/// receives `phase_code`. Uses the raw PAC singleton
/// (`embassy_rp::pac::WATCHDOG`) which does not require peripheral
/// ownership, matching the pattern used by the safe `Watchdog` wrapper
/// internally.
fn write_breadcrumb_phase(offset: u32, phase_code: u32) {
    let watchdog = pac::WATCHDOG;
    watchdog.scratch0().write_value(OTA_BREADCRUMB_MAGIC);
    watchdog.scratch1().write_value(offset);
    watchdog.scratch2().write_value(phase_code);
}

/// Clears the OTA breadcrumb from SCRATCH0 and SCRATCH2.
///
/// Called by `finish()` after the flash operation succeeds, so a successful
/// OTA reboot does not look like a mid-upload watchdog reset to the next boot.
fn clear_breadcrumb() {
    let watchdog = pac::WATCHDOG;
    watchdog.scratch0().write_value(0u32);
    watchdog.scratch2().write_value(0u32);
}

/// Records a panic breadcrumb in SCRATCH2 without touching SCRATCH0 or SCRATCH1.
///
/// Called from the custom panic handler. Writes `WRITE_PHASE_PANIC` into
/// SCRATCH2 so the next boot can report that a panic preceded the reset,
/// distinguishing it from a slow-flash watchdog timeout. Safe to call at any
/// time — an active OTA upload has its magic in SCRATCH0 already; calling
/// this outside an OTA session leaves SCRATCH0 at whatever value it held,
/// which is harmless. Uses only raw register writes with no allocation,
/// no locks, and no operations that could themselves panic.
pub fn record_panic_breadcrumb() {
    pac::WATCHDOG.scratch2().write_value(WRITE_PHASE_PANIC);
}

/// Partition type for the DFU and STATE regions: a [`BlockingPartition`]
/// over the shared blocking [`Flash`] driver, guarded by the cross-core
/// [`CriticalSectionRawMutex`].
type FlashPartition<const FLASH_SIZE: usize> = BlockingPartition<
    'static,
    CriticalSectionRawMutex,
    Flash<'static, FLASH, Blocking, FLASH_SIZE>,
>;

/// Scratch buffer length the updater uses to read and rewrite the STATE
/// magic.
///
/// `BlockingFirmwareState::new` asserts this equals `STATE::WRITE_SIZE`.
/// The RP MCU blocking flash, and therefore the `BlockingPartition` over
/// it, has a one-byte write size, so the buffer is one byte.
pub(crate) const STATE_SCRATCH_LEN: usize = 1;

/// Maps a [`FirmwareUpdaterError`] to the coarser [`ubos_api::OtaError`].
///
/// `embassy-boot` is swap-based and has no partition table, so
/// [`ApiError::NoPartitionTable`] never applies. A bad bootloader state
/// means the running image was never marked booted; both that case and any
/// flash failure surface as [`ApiError::Flash`]. An out-of-bounds partition
/// write is the DFU-overflow case and maps to [`ApiError::SlotOverflow`].
fn map_updater_error(error: FirmwareUpdaterError) -> ApiError {
    match error {
        FirmwareUpdaterError::Flash(
            embedded_storage::nor_flash::NorFlashErrorKind::OutOfBounds,
        ) => ApiError::SlotOverflow,
        other => {
            warn!("OTA updater error: {:?}", defmt::Debug2Format(&other));
            ApiError::Flash
        }
    }
}

/// Builds the DFU and STATE partitions from the linker symbols.
///
/// The `__bootloader_*` symbols are flash offsets relative to the XIP
/// base; `BlockingPartition` operates in those same offset units over
/// the full-flash driver, so the partition windows land exactly on the
/// bootloader's DFU and STATE regions.
fn partitions<const FLASH_SIZE: usize>(
    flash: &'static SharedFlashMutex<FLASH_SIZE>,
) -> (FlashPartition<FLASH_SIZE>, FlashPartition<FLASH_SIZE>) {
    unsafe extern "C" {
        static __bootloader_state_start: u32;
        static __bootloader_state_end: u32;
        static __bootloader_dfu_start: u32;
        static __bootloader_dfu_end: u32;
    }

    // Reading a linker-defined symbol's address is the documented way
    // to obtain these offsets; the values are fixed at link time.
    let dfu_start = core::ptr::addr_of!(__bootloader_dfu_start) as u32;
    let dfu_end = core::ptr::addr_of!(__bootloader_dfu_end) as u32;
    let state_start = core::ptr::addr_of!(__bootloader_state_start) as u32;
    let state_end = core::ptr::addr_of!(__bootloader_state_end) as u32;

    let dfu = BlockingPartition::new(flash, dfu_start, dfu_end - dfu_start);
    let state = BlockingPartition::new(flash, state_start, state_end - state_start);
    (dfu, state)
}

/// Builds a [`BlockingFirmwareUpdater`] over freshly-made partitions.
///
/// The `scratch` borrow ties the updater's lifetime to the scratch buffer,
/// so the updater is short-lived: each public method builds one, performs
/// its flash work, and drops it.
fn build_updater<'a, const FLASH_SIZE: usize>(
    flash: &'static SharedFlashMutex<FLASH_SIZE>,
    scratch: &'a mut [u8; STATE_SCRATCH_LEN],
) -> BlockingFirmwareUpdater<'a, FlashPartition<FLASH_SIZE>, FlashPartition<FLASH_SIZE>> {
    let (dfu, state) = partitions(flash);
    BlockingFirmwareUpdater::new(FirmwareUpdaterConfig { dfu, state }, scratch)
}

/// Requests the core-1 park and spin-waits until the mining worker
/// acknowledges, bounded by [`PARK_POLL_MAX_ATTEMPTS`].
///
/// Returns `true` once core 1 is parked, or `false` on timeout. Used by
/// every flash-writing path so `embassy-rp`'s `pause_core1()` handshake
/// always finds the worker idle; without the pre-emptive park the
/// handshake stalls >15 s when core 1's executor is mid-`AtomicU64`
/// seqlock (the `portable-atomic` fallback disables interrupts, blocking
/// the `SIO_IRQ_FIFO` pause handshake), tripping the watchdog before the
/// write completes. The caller releases the park; this only acquires and
/// waits.
///
/// The park is re-requested on every poll iteration. A single cross-core
/// wake can be missed when core 1's executor is idle in `wfe` and the
/// `SEV` lands during a brief interrupts-disabled window; re-signalling
/// each iteration guarantees the worker is woken and parks well within the
/// budget. The total spin budget stays far under the 2 s watchdog.
///
/// # Why the 50 ms ceiling must not be widened
///
/// This function is a synchronous busy-spin executing on core 0's
/// embassy executor. Spinning longer would starve the unconditional
/// watchdog-feed task on core 0, causing the 2 s hardware watchdog to
/// fire — the opposite of the intended fix. The 50 ms budget is safe
/// because the OTA stop-preflight (`ubos-manager` `stop_mining_and_wait`)
/// calls `SERIAL.reset()` before cutting the hashboard rail, leaving the
/// core-1 serial worker idle between commands by the time `begin()` runs.
/// Under that condition the ack arrives well within the first millisecond,
/// so the budget is never approached in practice. If the park does time
/// out, `begin()` returns an error and the handler fails closed, resuming
/// mining rather than proceeding to a flash write.
fn park_core1_and_wait(delay_cycles: u32) -> bool {
    let mut attempts = 0u32;
    loop {
        ubos_mining::request_core1_park();
        if ubos_mining::core1_is_parked() {
            return true;
        }
        if attempts >= PARK_POLL_MAX_ATTEMPTS {
            return false;
        }
        cortex_m::asm::delay(delay_cycles);
        attempts += 1;
    }
}

/// RP OTA target backed by `embassy-boot`.
///
/// Holds the shared flash mutex, the firmware version for the running image,
/// a scratch buffer for the STATE partition, and the running write offset
/// into the DFU partition. The [`BlockingFirmwareUpdater`] is rebuilt for
/// each operation: it owns only the partition handles and the scratch buffer,
/// both of which are cheap to reconstruct, and the DFU write offset is
/// tracked here rather than inside the updater so a fresh updater never
/// re-erases an already-written sector.
///
/// Generic over `FLASH_SIZE` (the total flash in bytes) and
/// `PARK_POLL_DELAY_CYCLES` (the cortex-m delay between each core-1 park
/// poll, tuned per board to approximate 100 µs at that board's clock rate).
pub struct RpOtaTarget<const FLASH_SIZE: usize, const PARK_POLL_DELAY_CYCLES: u32> {
    /// Shared blocking flash, partitioned per-op into DFU and STATE.
    flash: &'static SharedFlashMutex<FLASH_SIZE>,
    /// Firmware version compiled into the running image.
    firmware_version: u32,
    /// Scratch buffer for the STATE partition magic; length must equal the
    /// partition `WRITE_SIZE`.
    scratch: [u8; STATE_SCRATCH_LEN],
    /// Next DFU write offset, advanced by each [`write_chunk`]. `None`
    /// until [`begin`] erases the DFU and arms the writer.
    ///
    /// [`write_chunk`]: ubos_api::OtaTarget::write_chunk
    /// [`begin`]: ubos_api::OtaTarget::begin
    write_offset: Option<usize>,
}

impl<const FLASH_SIZE: usize, const PARK_POLL_DELAY_CYCLES: u32>
    RpOtaTarget<FLASH_SIZE, PARK_POLL_DELAY_CYCLES>
{
    /// Creates a target over the shared flash mutex with no active write.
    #[must_use]
    pub const fn new(flash: &'static SharedFlashMutex<FLASH_SIZE>, firmware_version: u32) -> Self {
        Self {
            flash,
            firmware_version,
            scratch: [0; STATE_SCRATCH_LEN],
            write_offset: None,
        }
    }

    /// Builds the DFU and STATE partitions for this target.
    fn partitions(&self) -> (FlashPartition<FLASH_SIZE>, FlashPartition<FLASH_SIZE>) {
        partitions(self.flash)
    }

    /// Builds a [`BlockingFirmwareUpdater`] over freshly-made partitions.
    fn updater(
        &mut self,
    ) -> BlockingFirmwareUpdater<'_, FlashPartition<FLASH_SIZE>, FlashPartition<FLASH_SIZE>> {
        build_updater(self.flash, &mut self.scratch)
    }
}

impl<const FLASH_SIZE: usize, const PARK_POLL_DELAY_CYCLES: u32> ubos_api::OtaTarget
    for RpOtaTarget<FLASH_SIZE, PARK_POLL_DELAY_CYCLES>
{
    /// Returns the bytes written since the last [`begin`] call.
    ///
    /// Exposes `write_offset` so the chunk-ack handler can validate each
    /// incoming chunk offset without holding any extra state outside the
    /// target. `None` means no session is active.
    ///
    /// [`begin`]: ubos_api::OtaTarget::begin
    fn bytes_written(&self) -> Option<usize> {
        self.write_offset
    }

    /// Returns the version compiled into the running firmware image.
    fn running_version(&self) -> u32 {
        self.firmware_version
    }

    /// Returns the running slot label.
    ///
    /// `embassy-boot` is swap-based with a single ACTIVE partition, so
    /// there is no A/B slot identity to report; always `"ACTIVE"`.
    fn running_slot_label(&self) -> &'static str {
        "ACTIVE"
    }

    /// Whether a revertable previous image exists in the DFU slot.
    ///
    /// `embassy-boot` keeps the prior image in DFU after a swap but stores
    /// no metadata marking it valid, so this inspects the DFU slot's first
    /// vector-table words: a staged image starts with an initial stack
    /// pointer in RAM and a thumb reset vector pointing into the ACTIVE
    /// window. Erased flash or leftover code fails both checks, so the
    /// revert path stays closed until an OTA has actually staged a swap.
    fn has_previous_image(&self) -> bool {
        // The ACTIVE window comes from the same linker symbols the bootloader
        // links against, and the stack top from cortex-m-rt's `_stack_start`,
        // so this tracks memory.x rather than hardcoding the layout. Only the
        // SRAM base is a fixed RP architectural address (0x2000_0000) that
        // applies to both RP2040 and RP2350; there is no linker symbol for it.
        const SRAM_BASE: u32 = 0x2000_0000;
        unsafe extern "C" {
            static __bootloader_active_start: u32;
            static __bootloader_active_end: u32;
            static _stack_start: u32;
        }
        let flash_base = embassy_rp::flash::FLASH_BASE as u32;
        let active_start = flash_base + core::ptr::addr_of!(__bootloader_active_start) as u32;
        let active_end = flash_base + core::ptr::addr_of!(__bootloader_active_end) as u32;
        let stack_top = core::ptr::addr_of!(_stack_start) as u32;

        let (mut dfu, _state) = self.partitions();
        let mut head = [0u8; 8];
        if dfu.read(0, &mut head).is_err() {
            return false;
        }
        let initial_sp = u32::from_le_bytes([head[0], head[1], head[2], head[3]]);
        let reset_vector = u32::from_le_bytes([head[4], head[5], head[6], head[7]]);
        (SRAM_BASE..=stack_top).contains(&initial_sp)
            && reset_vector & 1 == 1
            && (active_start..active_end).contains(&(reset_vector & !1))
    }

    /// Version of the previous image, if any.
    ///
    /// Always `None`: `embassy-boot` stores no per-image version. See
    /// [`has_previous_image`](Self::has_previous_image).
    fn previous_version(&self) -> Option<u32> {
        None
    }

    /// Arms the DFU writer after checking the running image is booted.
    ///
    /// Deliberately does not call `prepare_update`: that erases the entire
    /// 956 KiB DFU partition in one blocking flash op, which starves the
    /// 2 s watchdog feeder and resets the board mid-update before the swap
    /// is ever marked. `write_firmware` (driven by [`write_chunk`]) erases
    /// each DFU sector lazily right before programming it, and the handler
    /// yields between chunks, so the bulk pre-erase is both redundant and
    /// harmful. Here we only verify the running image is in the booted
    /// state, matching the guard `prepare_update` applied, and arm the
    /// write offset at zero.
    ///
    /// Parks core 1 before proceeding so `embassy-rp`'s `pause_core1()`
    /// FIFO handshake inside `write_firmware` always finds the worker idle.
    /// Without the pre-emptive park the handshake stalls >15 s in ~1 in 30
    /// chunk writes when the mining serial worker is blocked in
    /// `BufferedUart::read`, causing a watchdog reset before the write
    /// completes.
    ///
    /// The OTA breadcrumb (SCRATCH0 = magic, SCRATCH1 = 0, SCRATCH2 = None)
    /// is written here so any subsequent watchdog timeout is identifiable at
    /// the next boot as an OTA-induced reset.
    ///
    /// [`write_chunk`]: ubos_api::OtaTarget::write_chunk
    fn begin(&mut self) -> Result<(), ApiError> {
        let mut updater = self.updater();
        match updater.get_state().map_err(map_updater_error)? {
            State::Boot => {}
            State::Swap => {
                warn!("OTA begin: previous image staged but not yet booted; reboot required");
                return Err(ApiError::Flash);
            }
            other => {
                warn!("OTA begin: image not in booted state: {:?}", other);
                return Err(ApiError::Flash);
            }
        }

        // Park core 1 before any flash access. The mining serial worker runs
        // on core 1 and may be blocked in `BufferedUart::read` when a flash
        // write arrives. Without this pre-emptive park, `embassy-rp`'s
        // `pause_core1()` handshake inside `write_firmware` must wait for the
        // worker to reach a `wfe` — which never fires when the UART ISR keeps
        // feeding the buffer — causing the handshake to stall until the
        // watchdog resets the board.
        if !park_core1_and_wait(PARK_POLL_DELAY_CYCLES) {
            warn!("OTA begin: core 1 did not park within timeout; aborting");
            ubos_mining::release_core1_park();
            return Err(ApiError::Flash);
        }

        self.write_offset = Some(0);
        write_breadcrumb_phase(0, WRITE_PHASE_NONE);
        Ok(())
    }

    /// Writes the next chunk of firmware bytes into the DFU partition.
    ///
    /// Core 1 is pre-parked in [`begin`] so `embassy-rp`'s `pause_core1()`
    /// FIFO handshake inside `write_firmware` always finds the worker idle,
    /// avoiding the >15 s stall that trips the watchdog when the handshake
    /// races a `portable-atomic` seqlock. The blocking flash has a one-byte
    /// write size, so unaligned chunk lengths are accepted; an out-of-bounds
    /// write past the DFU end surfaces as [`ApiError::SlotOverflow`].
    ///
    /// [`begin`]: ubos_api::OtaTarget::begin
    fn write_chunk(&mut self, data: &[u8]) -> Result<(), ApiError> {
        let Some(offset) = self.write_offset else {
            return Err(ApiError::Flash);
        };
        // A fresh updater is built per call. `BlockingFirmwareUpdater` tracks
        // which DFU sectors have been erased internally and erases each sector
        // once, right before its first write. Building a new updater resets
        // that tracking, so correctness requires that each call writes into a
        // region the previous call did not reach — equivalently, that the
        // chunk size equals the flash erase size. The ubos-api handler sends
        // exactly `OTA_CHUNK_SIZE` (4096 bytes) per call, which matches the
        // RP flash erase size of 4096 bytes. If that chunk size ever
        // changes to be smaller than the erase size, a second write into an
        // already-erased sector will trigger a redundant erase from the fresh
        // updater, corrupting the previously written data.
        debug_assert_eq!(
            offset % flash::ERASE_SIZE,
            0,
            "OTA chunk must start on a flash erase-sector boundary"
        );
        let mut updater = self.updater();
        // Set IN_FLASH before the write call so a watchdog timeout inside
        // `write_firmware` is distinguishable from one between writes.
        #[allow(clippy::cast_possible_truncation)]
        write_breadcrumb_phase(offset as u32, WRITE_PHASE_IN_FLASH);
        updater
            .write_firmware(offset, data)
            .map_err(map_updater_error)?;
        let new_offset = offset + data.len();
        self.write_offset = Some(new_offset);
        // Set FLASH_DONE after the write returns so the next boot knows the
        // write succeeded and the watchdog fired between chunks, not inside
        // one. Update SCRATCH1 with the new offset for post-mortem diagnostics.
        #[allow(clippy::cast_possible_truncation)]
        write_breadcrumb_phase(new_offset as u32, WRITE_PHASE_FLASH_DONE);
        Ok(())
    }

    /// Marks the DFU image for swap on the next boot.
    ///
    /// Flips the STATE partition magic to `Swap`; the bootloader performs
    /// the active/DFU exchange when the device next resets. Clears the OTA
    /// breadcrumb so the subsequent `reboot_into_new()` is not mistaken for
    /// a mid-upload watchdog reset at the next boot. Releases the core-1
    /// park unconditionally so a subsequent reboot (or a failure path that
    /// calls `abort()`) does not leave the worker spinning.
    fn finish(&mut self) -> Result<(), ApiError> {
        if self.write_offset.is_none() {
            ubos_mining::release_core1_park();
            return Err(ApiError::Flash);
        }
        let mut updater = self.updater();
        let result = updater.mark_updated().map_err(map_updater_error);
        self.write_offset = None;
        // Clear before returning so a failed finish still has the breadcrumb
        // cleared — on failure the board does not reboot, so no diagnostic
        // is lost; on success the reboot that follows must not be misread.
        clear_breadcrumb();
        // Release the park after the breadcrumb is cleared. On success the
        // reboot follows immediately; on failure `abort()` is called next,
        // but it is idempotent so releasing here is safe.
        ubos_mining::release_core1_park();
        result
    }

    /// Releases the core-1 park and clears the OTA breadcrumb without
    /// committing the update.
    ///
    /// Called by the REST handler on every error or abandoned-upload path
    /// after [`begin`] succeeded, so a failed session leaves no parked
    /// core and no stale breadcrumb. Idempotent: calling it when the park
    /// was already released or no session is active is safe.
    ///
    /// [`begin`]: ubos_api::OtaTarget::begin
    fn abort(&mut self) {
        ubos_mining::release_core1_park();
        clear_breadcrumb();
        self.write_offset = None;
    }

    /// Resets the MCU so the bootloader swaps in the new image.
    ///
    /// Releases the core-1 park before resetting so the reset does not
    /// carry stale `PARK_REQUESTED` state into the next boot image
    /// (the statics are in RAM and survive a warm reset). Never returns.
    /// The swap happens in the bootloader on the next boot; the HTTP
    /// handler flushes its acknowledgement before calling this.
    fn reboot_into_new(&mut self) -> ! {
        ubos_mining::release_core1_park();
        cortex_m::peripheral::SCB::sys_reset()
    }
}

/// Logs the bootloader state at startup and marks the running image booted.
///
/// Call once early in boot. A [`State::Swap`] prior state means the
/// bootloader just swapped in an OTA image; marking it booted stops the
/// bootloader from reverting on the next reset, confirming the update.
pub fn mark_booted<const FLASH_SIZE: usize>(flash: &'static SharedFlashMutex<FLASH_SIZE>) {
    let mut scratch = [0u8; STATE_SCRATCH_LEN];
    let mut updater = build_updater(flash, &mut scratch);
    match updater.get_state() {
        Ok(State::Swap) => info!("OTA boot: bootloader swapped in a new image, confirming"),
        Ok(state) => info!("OTA boot: bootloader state {:?}", state),
        Err(error) => warn!(
            "OTA boot: get_state failed: {:?}",
            defmt::Debug2Format(&error)
        ),
    }
    if let Err(error) = updater.mark_booted() {
        warn!(
            "OTA boot: mark_booted failed: {:?}",
            defmt::Debug2Format(&error)
        );
    }
}
