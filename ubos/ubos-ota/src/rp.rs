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

//! RP backend for the OTA engine, shared by all RP board firmware crates.
//!
//! [`RpBackend`] implements [`crate::OtaBackend`] on top of `embassy-boot`'s
//! [`BlockingFirmwareUpdater`]. The updater writes the incoming image into the
//! DFU partition and flips the bootloader state partition's magic; the RP
//! bootloader performs the active/DFU swap on the next boot. The backend is
//! wrapped in an [`OtaEngine`](crate::OtaEngine), constructed once at boot,
//! registered with [`ubos_api::register_ota_target`], and then driven through
//! the REST handler's `dyn OtaTarget` reference.
//!
//! The DFU and STATE partitions are [`BlockingPartition`]s over the one
//! shared `'static` flash mutex (see [`crate::shared_flash`]). Their offsets
//! and sizes come from the `__bootloader_dfu_*` / `__bootloader_state_*`
//! linker symbols defined in `memory.x`, matching the layout the bootloader
//! links against.
//!
//! Core 1 runs the ubos-mining serial worker. Every flash-writing path must
//! run with core 1 parked: `embassy-rp`'s `pause_core1()` handshake stalls for
//! over 15 s when core 1's executor is mid-`AtomicU64` seqlock (the
//! `portable-atomic` fallback disables interrupts, blocking the
//! `SIO_IRQ_FIFO` pause handshake), tripping the watchdog before the write
//! completes. The [`OtaEngine`](crate::OtaEngine) parks core 1 before the
//! first flash access and holds the park across the whole upload, polling
//! [`RpBackend::park_poll_delay`] between park-ack checks.
//!
//! # OTA breadcrumb
//!
//! The watchdog SCRATCH0, SCRATCH1, and SCRATCH2 registers survive a warm
//! reset (watchdog timeout, `SCB::sys_reset`) but are cleared by a power-on
//! reset. SCRATCH0–3 are free for firmware use; the RP bootrom reserves
//! SCRATCH4–7 for its watchdog-boot vectoring, and embassy-boot-rp itself
//! uses no scratch registers, so SCRATCH0–2 are safe to use.
//!
//! The engine drives the breadcrumb through [`RpBackend::write_breadcrumb`]
//! and [`RpBackend::clear_breadcrumb`]: SCRATCH0 receives the magic, SCRATCH1
//! the offset, and SCRATCH2 the write phase. The engine writes `None` at
//! session start, `InFlash` before each write and `FlashDone` after, so a
//! watchdog timeout inside a write is distinguishable from one between writes,
//! and clears SCRATCH0/SCRATCH2 on finish so a successful update does not look
//! like a mid-upload reset. [`record_panic_breadcrumb`] writes
//! `WRITE_PHASE_PANIC` into SCRATCH2 unconditionally from the panic handler so
//! any panic — even one outside an OTA upload — is recorded and survives the
//! subsequent watchdog reset. If the board resets mid-upload, main.rs reads
//! the magic, offset, and phase at boot and publishes them as reset
//! diagnostics so the cause is visible over HTTP without a debug probe.

use defmt::{info, warn};
use embassy_boot::FirmwareUpdaterError;
use embassy_boot_rp::{BlockingFirmwareUpdater, FirmwareUpdaterConfig, State};
use embassy_embedded_hal::flash::partition::BlockingPartition;
use embassy_rp::flash::{self, Blocking, Flash};
use embassy_rp::pac;
use embassy_rp::peripherals::FLASH;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embedded_storage::nor_flash::ReadNorFlash;
use ubos_api::{OtaError as ApiError, OtaWritePhase};

use crate::OtaBackend;
use crate::shared_flash::{FlashGuard, SharedFlash};

/// The RP blocking flash driver over the one QSPI `FLASH` peripheral.
pub type RpFlash<const FLASH_SIZE: usize> = Flash<'static, FLASH, Blocking, FLASH_SIZE>;

/// Type of the `'static` mutex guarding the one blocking [`Flash`] driver.
///
/// Exposed so `main` can declare the backing `static` and so the OTA updater
/// can build `BlockingPartition`s over the very same mutex.
pub type SharedFlashMutex<const FLASH_SIZE: usize> =
    crate::shared_flash::SharedFlashMutex<RpFlash<FLASH_SIZE>>;

/// `Copy` handle to the shared blocking [`Flash`] driver, parking the core-1
/// serial worker around each write and erase.
pub type SharedBlockingFlash<const FLASH_SIZE: usize> = SharedFlash<RpFlash<FLASH_SIZE>, RpGuard>;

/// Cap on the core-1 park-ack busy-spin, after which the flash operation is
/// abandoned. The worker parks within one loop iteration once signalled,
/// but a bring-up command holds it for seconds, so this bound is reached in
/// normal operation whenever a save collides with one.
const MAX_PARK_SPINS: u32 = 5_000_000;

/// Spin count between park re-signals. A single cross-core wake can be lost
/// when core 1 is in `wfe` with interrupts briefly disabled, and without a
/// retry the whole budget would be spent waiting on a wake that never
/// arrives. A power of two so the check is a mask.
const PARK_RESIGNAL_SPINS: u32 = 0xFFFF;

/// [`FlashGuard`] that parks the core-1 serial worker for the span of a
/// blocking write or erase.
///
/// `embassy-rp`'s blocking `Flash` already pauses core 1 for the XIP-off
/// window (`in_ram` → `multicore::pause_core1`), but that pause `wfe`s in a
/// flash-resident handler: a pending chip-UART RX interrupt wakes it and
/// faults core 1 fetching from suspended flash. Parking the worker first
/// makes it mask its RX interrupt (see `BaudRate::set_rx_irq_enabled`), so no
/// interrupt pends to wake the pause.
///
/// Fails closed. Running the operation unparked is the exact hardfault this
/// exists to prevent, so a park that does not ack within [`MAX_PARK_SPINS`]
/// gives up and returns [`flash::Error::Other`]: the caller sees a failed
/// write instead of the board rebooting under it. A config save colliding
/// with a chain bring-up is the case that reaches this, since the worker
/// cannot answer a park until the command in flight finishes.
///
/// When no worker is running the park is skipped rather than waited out. A
/// worker that exited on serial EOF never acks again, and has already masked
/// its RX interrupt on the way out, so waiting would fail every later save for
/// nothing.
#[derive(Clone, Copy)]
pub struct RpGuard(&'static ubos_mining::Park);

impl<const FLASH_SIZE: usize> SharedBlockingFlash<FLASH_SIZE> {
    /// Wraps the `'static` flash mutex in a handle that parks through `park`
    /// around each write and erase. The board also hands `park` to its serial
    /// actors, so that both sides meet at the same counters. It is the one
    /// constructor of the guard, so an RP flash handle always parks.
    #[must_use]
    pub const fn new(
        mutex: &'static SharedFlashMutex<FLASH_SIZE>,
        park: &'static ubos_mining::Park,
    ) -> Self {
        Self::with_guard(mutex, RpGuard(park))
    }
}

impl FlashGuard<flash::Error> for RpGuard {
    fn guard<R>(&self, op: impl FnOnce() -> Result<R, flash::Error>) -> Result<R, flash::Error> {
        let park = self.0;
        if !park.has_workers() {
            return op();
        }

        let _request = park.hold();
        let mut spins = 0u32;
        while !park.is_parked() {
            if spins >= MAX_PARK_SPINS {
                return Err(flash::Error::Other);
            }
            core::hint::spin_loop();
            spins += 1;
            if spins & PARK_RESIGNAL_SPINS == 0 {
                park.resignal();
            }
        }
        op()
    }
}

/// Magic value written to SCRATCH0 when an OTA upload session is active.
///
/// The value is nonzero and distinctive enough to survive accidental
/// coincidence with uninitialized RAM. Cleared to zero on finish so a
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
const _: () = assert!(WRITE_PHASE_NONE == OtaWritePhase::None as u32);
const _: () = assert!(WRITE_PHASE_IN_FLASH == OtaWritePhase::InFlash as u32);
const _: () = assert!(WRITE_PHASE_FLASH_DONE == OtaWritePhase::FlashDone as u32);
const _: () = assert!(WRITE_PHASE_PANIC == OtaWritePhase::Panic as u32);

// `write` builds a fresh `BlockingFirmwareUpdater` per call, which erases each
// DFU sector once before its first write. Correctness requires every chunk to
// cover exactly one erase sector, so the API's chunk size must equal the flash
// erase size. A smaller chunk would land the next write mid-sector and a fresh
// updater would re-erase it, dropping the bytes already written. The runtime
// `debug_assert_eq!` in `write` catches the resulting misaligned offset in
// debug builds; this guards the coupling at compile time in every build.
const _: () = assert!(ubos_api::system::OTA_CHUNK_SIZE == flash::ERASE_SIZE);

/// Maps an [`OtaWritePhase`] to the raw SCRATCH2 code.
fn phase_code(phase: OtaWritePhase) -> u32 {
    match phase {
        OtaWritePhase::None => WRITE_PHASE_NONE,
        OtaWritePhase::InFlash => WRITE_PHASE_IN_FLASH,
        OtaWritePhase::FlashDone => WRITE_PHASE_FLASH_DONE,
        OtaWritePhase::Panic => WRITE_PHASE_PANIC,
    }
}

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
/// Called on finish after the flash operation succeeds, so a successful OTA
/// reboot does not look like a mid-upload watchdog reset to the next boot.
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
/// so the updater is short-lived: each method builds one, performs its flash
/// work, and drops it.
fn build_updater<'a, const FLASH_SIZE: usize>(
    flash: &'static SharedFlashMutex<FLASH_SIZE>,
    scratch: &'a mut [u8; STATE_SCRATCH_LEN],
) -> BlockingFirmwareUpdater<'a, FlashPartition<FLASH_SIZE>, FlashPartition<FLASH_SIZE>> {
    let (dfu, state) = partitions(flash);
    BlockingFirmwareUpdater::new(FirmwareUpdaterConfig { dfu, state }, scratch)
}

/// RP OTA backend backed by `embassy-boot`.
///
/// Holds the shared flash mutex and a scratch buffer for the STATE partition.
/// The [`BlockingFirmwareUpdater`] is rebuilt for each operation: it owns only
/// the partition handles and the scratch buffer, both cheap to reconstruct,
/// and the DFU write offset is tracked by the [`OtaEngine`](crate::OtaEngine)
/// rather than inside the updater so a fresh updater never re-erases an
/// already-written sector.
///
/// Generic over `FLASH_SIZE` (the total flash in bytes) and
/// `PARK_POLL_DELAY_CYCLES` (the cortex-m delay between each core-1 park
/// poll, tuned per board to approximate 100 µs at that board's clock rate).
pub struct RpBackend<const FLASH_SIZE: usize, const PARK_POLL_DELAY_CYCLES: u32> {
    /// Shared blocking flash, partitioned per-op into DFU and STATE.
    flash: &'static SharedFlashMutex<FLASH_SIZE>,
    /// Scratch buffer for the STATE partition magic; length must equal the
    /// partition `WRITE_SIZE`.
    scratch: [u8; STATE_SCRATCH_LEN],
}

impl<const FLASH_SIZE: usize, const PARK_POLL_DELAY_CYCLES: u32>
    RpBackend<FLASH_SIZE, PARK_POLL_DELAY_CYCLES>
{
    /// Creates a backend over the shared flash mutex.
    #[must_use]
    pub const fn new(flash: &'static SharedFlashMutex<FLASH_SIZE>) -> Self {
        Self {
            flash,
            scratch: [0; STATE_SCRATCH_LEN],
        }
    }

    /// Builds the DFU and STATE partitions for this backend.
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

impl<const FLASH_SIZE: usize, const PARK_POLL_DELAY_CYCLES: u32> OtaBackend
    for RpBackend<FLASH_SIZE, PARK_POLL_DELAY_CYCLES>
{
    /// Verifies the running image is in the booted state.
    ///
    /// Deliberately does not call `prepare_update`: that erases the entire
    /// 956 KiB DFU partition in one blocking flash op, which starves the
    /// 2 s watchdog feeder and resets the board mid-update before the swap
    /// is ever marked. `write_firmware` (driven by the engine's `write`)
    /// erases each DFU sector lazily right before programming it, and the
    /// handler yields between chunks, so the bulk pre-erase is both redundant
    /// and harmful. Here we only verify the running image is in the booted
    /// state, matching the guard `prepare_update` applied.
    fn prepare(&mut self) -> Result<(), ApiError> {
        let mut updater = self.updater();
        match updater.get_state().map_err(map_updater_error)? {
            State::Boot => Ok(()),
            State::Swap => {
                warn!("OTA begin: previous image staged but not yet booted; reboot required");
                Err(ApiError::Flash)
            }
            other => {
                warn!("OTA begin: image not in booted state: {:?}", other);
                Err(ApiError::Flash)
            }
        }
    }

    /// Writes the chunk into the DFU partition at `offset`.
    ///
    /// A fresh updater is built per call. `BlockingFirmwareUpdater` tracks
    /// which DFU sectors have been erased internally and erases each sector
    /// once, right before its first write. Building a new updater resets that
    /// tracking, so correctness requires that each call writes into a region
    /// the previous call did not reach — equivalently, that the chunk size
    /// equals the flash erase size (enforced by the compile-time assert
    /// above). The blocking flash has a one-byte write size, so unaligned
    /// chunk lengths are accepted; an out-of-bounds write past the DFU end
    /// surfaces as [`ApiError::SlotOverflow`].
    fn write(&mut self, offset: usize, data: &[u8]) -> Result<(), ApiError> {
        debug_assert_eq!(
            offset % flash::ERASE_SIZE,
            0,
            "OTA chunk must start on a flash erase-sector boundary"
        );
        let mut updater = self.updater();
        updater
            .write_firmware(offset, data)
            .map_err(map_updater_error)
    }

    /// Marks the DFU image for swap on the next boot.
    ///
    /// Flips the STATE partition magic to `Swap`; the bootloader performs the
    /// active/DFU exchange when the device next resets.
    fn activate(&mut self) -> Result<(), ApiError> {
        let mut updater = self.updater();
        updater.mark_updated().map_err(map_updater_error)
    }

    /// Resets the MCU so the bootloader swaps in the new image. Never returns.
    fn reboot(&mut self) -> ! {
        cortex_m::peripheral::SCB::sys_reset()
    }

    /// Returns the running slot label.
    ///
    /// `embassy-boot` is swap-based with a single ACTIVE partition, so there
    /// is no A/B slot identity to report; always `"ACTIVE"`.
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

    fn write_breadcrumb(&self, offset: u32, phase: OtaWritePhase) {
        write_breadcrumb_phase(offset, phase_code(phase));
    }

    fn clear_breadcrumb(&self) {
        clear_breadcrumb();
    }

    /// Spins one park-poll interval via the cortex-m delay loop, sized per
    /// board by `PARK_POLL_DELAY_CYCLES` to approximate 100 µs.
    fn park_poll_delay(&self) {
        cortex_m::asm::delay(PARK_POLL_DELAY_CYCLES);
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
