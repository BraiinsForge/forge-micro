//! ESP backend for the OTA engine.
//!
//! [`EspBackend`] implements [`crate::OtaBackend`] over the esp-idf OTA
//! partition scheme via `esp-bootloader-esp-idf`'s [`OtaUpdater`]. The incoming
//! image is written into the inactive `ota_N` app slot; [`activate`] flips the
//! `otadata` partition to boot it and marks it [`OtaImageState::New`], so the
//! rollback-enabled bootloader boots it once as `PendingVerify` and reverts on
//! the next reset unless the firmware confirms it. [`confirm_running`] performs
//! that confirmation at boot.
//!
//! Unlike the RP backend there is no per-sector updater dance: `esp-storage`'s
//! [`embedded_storage::Storage::write`] does read-modify-erase-write at sector
//! granularity, so a sequential stream of arbitrary-sized chunks lands
//! correctly. The partition-table scratch buffer lives in the backend rather
//! than on the stack because each flash op already needs `esp-storage`'s 4 KiB
//! sector buffer on the stack.
//!
//! Core 1 runs the ubos-mining serial worker; the [`OtaEngine`](crate::OtaEngine)
//! parks it before any flash access, polling [`EspBackend::park_poll_delay`]
//! between park-ack checks.
//!
//! [`activate`]: crate::OtaBackend::activate

use core::cell::RefCell;

use defmt::{info, warn};
use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embedded_storage::nor_flash::{ErrorType, NorFlash, ReadNorFlash};
use embedded_storage::{ReadStorage, Storage};
use esp_bootloader_esp_idf::ota::OtaImageState;
use esp_bootloader_esp_idf::ota_updater::OtaUpdater;
use esp_bootloader_esp_idf::partitions::{
    AppPartitionSubType, Error as PtError, PARTITION_TABLE_MAX_LEN,
};
use ubos_api::{OtaError as ApiError, OtaWritePhase};

/// Microseconds spun per core-1 park-ack poll. 500 polls × 100 µs is the
/// engine's 50 ms park budget.
const PARK_POLL_DELAY_US: u32 = 100;

/// Type of the `'static` mutex guarding the one `esp-storage` flash driver.
///
/// The board has a single flash peripheral but two consumers — the
/// `ubos-config` record store and the OTA writer — so both go through this
/// one mutex. Exposed so `main` can declare the backing `static`.
pub type EspFlashMutex =
    Mutex<CriticalSectionRawMutex, RefCell<esp_storage::FlashStorage<'static>>>;

/// `Copy` handle to the shared `esp-storage` flash driver.
///
/// Implements the synchronous `embedded-storage` traits by locking the mutex
/// for the span of each operation: the OTA writer needs
/// [`embedded_storage::Storage`] (via [`OtaUpdater`]) and the config store
/// needs [`embedded_storage::nor_flash::NorFlash`] (via `ubos_config::SyncFlash`).
#[derive(Clone, Copy)]
pub struct EspSharedFlash {
    mutex: &'static EspFlashMutex,
}

impl EspSharedFlash {
    /// Wraps the `'static` flash mutex in a fresh `Copy` handle.
    #[must_use]
    pub const fn new(mutex: &'static EspFlashMutex) -> Self {
        Self { mutex }
    }
}

// `esp_storage::FlashStorage` implements both the `Storage` and `NorFlash`
// trait families, so every delegating call is fully qualified to avoid the
// ambiguity between the same-named methods.
type Inner = esp_storage::FlashStorage<'static>;

impl ReadStorage for EspSharedFlash {
    type Error = esp_storage::FlashStorageError;

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        self.mutex
            .lock(|cell| ReadStorage::read(&mut *cell.borrow_mut(), offset, bytes))
    }

    fn capacity(&self) -> usize {
        self.mutex
            .lock(|cell| ReadStorage::capacity(&*cell.borrow()))
    }
}

impl Storage for EspSharedFlash {
    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        self.mutex
            .lock(|cell| Storage::write(&mut *cell.borrow_mut(), offset, bytes))
    }
}

impl ErrorType for EspSharedFlash {
    type Error = esp_storage::FlashStorageError;
}

impl ReadNorFlash for EspSharedFlash {
    const READ_SIZE: usize = <Inner as ReadNorFlash>::READ_SIZE;

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        self.mutex
            .lock(|cell| ReadNorFlash::read(&mut *cell.borrow_mut(), offset, bytes))
    }

    fn capacity(&self) -> usize {
        self.mutex
            .lock(|cell| ReadNorFlash::capacity(&*cell.borrow()))
    }
}

impl NorFlash for EspSharedFlash {
    const WRITE_SIZE: usize = <Inner as NorFlash>::WRITE_SIZE;
    const ERASE_SIZE: usize = <Inner as NorFlash>::ERASE_SIZE;

    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        self.mutex
            .lock(|cell| NorFlash::write(&mut *cell.borrow_mut(), offset, bytes))
    }

    fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        self.mutex
            .lock(|cell| NorFlash::erase(&mut *cell.borrow_mut(), from, to))
    }
}

/// Maps an `esp-bootloader-esp-idf` partition error to the coarser
/// [`ubos_api::OtaError`].
///
/// A missing or malformed partition table (or one without two OTA slots)
/// surfaces as [`ApiError::NoPartitionTable`]; an out-of-bounds write past the
/// slot is the overflow case; everything else is a generic flash failure.
fn map_error(error: PtError) -> ApiError {
    match error {
        PtError::OutOfBounds => ApiError::SlotOverflow,
        PtError::Invalid | PtError::InvalidPartition { .. } | PtError::InvalidArgument => {
            ApiError::NoPartitionTable
        }
        other => {
            warn!("OTA esp error: {:?}", defmt::Debug2Format(&other));
            ApiError::Flash
        }
    }
}

/// Human-readable label for an app slot, reported in the OTA info endpoint.
fn slot_label(slot: AppPartitionSubType) -> &'static str {
    match slot {
        AppPartitionSubType::Factory => "factory",
        AppPartitionSubType::Ota0 => "ota_0",
        AppPartitionSubType::Ota1 => "ota_1",
        _ => "ota_n",
    }
}

/// ESP OTA backend over the esp-idf `otadata` two-slot scheme.
///
/// Holds the shared flash handle, the label of the slot the running image
/// booted from (queried once at construction), and the partition-table scratch
/// buffer the [`OtaUpdater`] reads into.
pub struct EspBackend {
    flash: EspSharedFlash,
    running_slot: &'static str,
    buffer: [u8; PARTITION_TABLE_MAX_LEN],
}

impl EspBackend {
    /// Creates a backend over the shared flash, recording which slot the
    /// running image booted from. A read failure leaves the label `"unknown"`
    /// rather than failing construction.
    #[must_use]
    pub fn new(flash: EspSharedFlash) -> Self {
        let mut probe = flash;
        let mut buffer = [0u8; PARTITION_TABLE_MAX_LEN];
        let running_slot = match OtaUpdater::new(&mut probe, &mut buffer) {
            Ok(mut updater) => match updater.selected_partition() {
                Ok(slot) => slot_label(slot),
                Err(_) => "unknown",
            },
            Err(_) => "unknown",
        };
        Self {
            flash,
            running_slot,
            buffer,
        }
    }
}

impl crate::OtaBackend for EspBackend {
    /// Validates the partition table (otadata plus at least two OTA slots) by
    /// constructing the updater. Erase happens lazily inside
    /// [`write`](Self::write) via `esp-storage`, so there is nothing to
    /// pre-erase here. Unlike the RP backend, this does not reject an update
    /// started during an unconfirmed `PendingVerify` boot: the esp-idf two-slot
    /// scheme always writes the new image into the inactive slot, so a re-flash
    /// before confirming is a valid recovery path rather than an error.
    fn prepare(&mut self) -> Result<(), ApiError> {
        let mut flash = self.flash;
        OtaUpdater::new(&mut flash, &mut self.buffer).map_err(map_error)?;
        Ok(())
    }

    /// Writes the chunk into the inactive OTA slot at `offset` (relative to the
    /// slot start). `esp-storage` erases each touched sector before writing, so
    /// a misaligned or sub-sector chunk is still correct.
    ///
    /// Rebuilding the `OtaUpdater` per chunk is safe: `next_partition` only
    /// reads the partition table and `otadata` to locate the inactive slot. The
    /// slot is not selected until [`activate`](Self::activate), so every chunk
    /// targets the same slot.
    fn write(&mut self, offset: usize, data: &[u8]) -> Result<(), ApiError> {
        let mut flash = self.flash;
        let mut updater = OtaUpdater::new(&mut flash, &mut self.buffer).map_err(map_error)?;
        let (mut region, _slot) = updater.next_partition().map_err(map_error)?;
        // FlashRegion implements both Storage and NorFlash; use the Storage
        // write, which read-modify-erase-writes each sector.
        #[allow(clippy::cast_possible_truncation)]
        Storage::write(&mut region, offset as u32, data).map_err(map_error)
    }

    /// Flips `otadata` to the freshly written slot and marks it
    /// [`OtaImageState::New`] so the bootloader boots it as `PendingVerify`,
    /// arming rollback until [`confirm_running`] confirms it.
    ///
    /// The `esp-bootloader-esp-idf` API exposes no atomic seq+state write, so
    /// this is two sector writes: the slot becomes bootable after the first and
    /// is marked `New` by the second. A power failure in that window leaves the
    /// new slot selected with a stale `ota_state`; the bootloader's two-slot
    /// fallback still boots a valid image, but rollback arming for this update
    /// is lost. Closing the window needs an upstream combined-write API.
    fn activate(&mut self) -> Result<(), ApiError> {
        let mut flash = self.flash;
        let mut updater = OtaUpdater::new(&mut flash, &mut self.buffer).map_err(map_error)?;
        updater.activate_next_partition().map_err(map_error)?;
        updater
            .set_current_ota_state(OtaImageState::New)
            .map_err(map_error)
    }

    /// Resets the chip; the bootloader boots the newly activated slot. Never
    /// returns.
    fn reboot(&mut self) -> ! {
        esp_hal::system::software_reset()
    }

    fn running_slot_label(&self) -> &'static str {
        self.running_slot
    }

    /// Always `false`: rollback is driven by the bootloader, not the app, and
    /// the esp-idf scheme records no app-visible previous-image marker.
    fn has_previous_image(&self) -> bool {
        false
    }

    fn previous_version(&self) -> Option<u32> {
        None
    }

    /// No-op: the ESP32-S3 has no reset-surviving scratch registers like the
    /// RP watchdog block, and the two-slot otadata scheme is self-healing, so
    /// no breadcrumb is recorded. Reset diagnostics fall back to the chip's
    /// reset reason.
    fn write_breadcrumb(&self, _offset: u32, _phase: OtaWritePhase) {}

    fn clear_breadcrumb(&self) {}

    fn park_poll_delay(&self) {
        esp_hal::delay::Delay::new().delay_micros(PARK_POLL_DELAY_US);
    }
}

/// Confirms the running image at boot so the bootloader stops arming rollback.
///
/// Call once early in boot. If the bootloader booted a freshly activated slot
/// it is in [`OtaImageState::PendingVerify`]; marking it [`OtaImageState::Valid`]
/// cancels the pending revert. Any other state (a normal boot of an already
/// confirmed image) is left untouched. Best-effort: a read failure is logged,
/// not propagated.
pub fn confirm_running(flash: EspSharedFlash) {
    let mut probe = flash;
    let mut buffer = [0u8; PARTITION_TABLE_MAX_LEN];
    let mut updater = match OtaUpdater::new(&mut probe, &mut buffer) {
        Ok(updater) => updater,
        Err(error) => {
            warn!(
                "OTA boot: updater init failed: {:?}",
                defmt::Debug2Format(&error)
            );
            return;
        }
    };
    match updater.current_ota_state() {
        Ok(OtaImageState::New | OtaImageState::PendingVerify) => {
            info!("OTA boot: image pending verify, confirming");
            if let Err(error) = updater.set_current_ota_state(OtaImageState::Valid) {
                warn!(
                    "OTA boot: confirm failed: {:?}",
                    defmt::Debug2Format(&error)
                );
            }
        }
        Ok(state) => info!("OTA boot: image state {:?}", defmt::Debug2Format(&state)),
        Err(error) => warn!(
            "OTA boot: get state failed: {:?}",
            defmt::Debug2Format(&error)
        ),
    }
}
