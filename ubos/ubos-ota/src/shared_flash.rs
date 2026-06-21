//! Shared blocking-flash adapter for the single RP MCU QSPI peripheral.
//!
//! The board has exactly one [`FLASH`](embassy_rp::peripherals::FLASH)
//! peripheral, but two independent consumers need it: the `ubos-config`
//! record store (writing the last two sectors) and the OTA
//! `BlockingFirmwareUpdater` (writing the DFU partition and the bootloader
//! state partition). Both must go through the same `embassy-rp` blocking
//! [`Flash`] driver, because a flash erase or program suspends XIP and runs
//! from RAM with core 1 paused; two independent driver instances over the
//! same peripheral would race that critical section.
//!
//! [`SharedBlockingFlash`] resolves this by holding the one [`Flash`]
//! instance in a `'static` [`Mutex`] of a [`RefCell`] and exposing a `Copy`
//! handle that implements the synchronous [`embedded_storage`] `NorFlash`
//! traits. The config backend takes this handle (through
//! [`ubos_config::SyncFlash`]); the OTA updater partitions the same
//! `'static` mutex directly via `embassy-embedded-hal`'s `BlockingPartition`.
//! The mutex is locked only for the duration of one synchronous flash
//! operation, never across an `await`, so it adds no latency beyond the
//! XIP-suspend the flash driver already performs.
//!
//! The mutex uses [`CriticalSectionRawMutex`] so the lock is sound against
//! both the other core and interrupt context, matching the cross-core
//! sharing the board relies on elsewhere.

use core::cell::RefCell;

use embassy_rp::flash::{Blocking, Flash};
use embassy_rp::peripherals::FLASH;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::blocking_mutex::Mutex;
use embedded_storage::nor_flash::{ErrorType, NorFlash, ReadNorFlash};

/// Type of the `'static` mutex guarding the one blocking [`Flash`] driver.
///
/// Exposed so `main` can declare the backing `static` and so the OTA
/// updater can build `BlockingPartition`s over the very same mutex.
pub type SharedFlashMutex<const FLASH_SIZE: usize> =
    Mutex<CriticalSectionRawMutex, RefCell<Flash<'static, FLASH, Blocking, FLASH_SIZE>>>;

/// `Copy` handle to the shared blocking [`Flash`] driver.
///
/// Implements the synchronous [`embedded_storage`] `NorFlash` traits by
/// locking the mutex for the span of each operation and delegating to the
/// inner [`Flash`]. Cheap to copy, so the config backend and any other
/// consumer can each hold one.
#[derive(Clone, Copy)]
pub struct SharedBlockingFlash<const FLASH_SIZE: usize> {
    mutex: &'static SharedFlashMutex<FLASH_SIZE>,
}

impl<const FLASH_SIZE: usize> SharedBlockingFlash<FLASH_SIZE> {
    /// Wraps the `'static` flash mutex in a fresh `Copy` handle.
    #[must_use]
    pub const fn new(mutex: &'static SharedFlashMutex<FLASH_SIZE>) -> Self {
        Self { mutex }
    }
}

impl<const FLASH_SIZE: usize> ErrorType for SharedBlockingFlash<FLASH_SIZE> {
    type Error = <Flash<'static, FLASH, Blocking, FLASH_SIZE> as ErrorType>::Error;
}

impl<const FLASH_SIZE: usize> ReadNorFlash for SharedBlockingFlash<FLASH_SIZE> {
    const READ_SIZE: usize =
        <Flash<'static, FLASH, Blocking, FLASH_SIZE> as ReadNorFlash>::READ_SIZE;

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        self.mutex
            .lock(|cell| cell.borrow_mut().read(offset, bytes))
    }

    fn capacity(&self) -> usize {
        self.mutex.lock(|cell| cell.borrow().capacity())
    }
}

impl<const FLASH_SIZE: usize> NorFlash for SharedBlockingFlash<FLASH_SIZE> {
    const WRITE_SIZE: usize = <Flash<'static, FLASH, Blocking, FLASH_SIZE> as NorFlash>::WRITE_SIZE;
    const ERASE_SIZE: usize = <Flash<'static, FLASH, Blocking, FLASH_SIZE> as NorFlash>::ERASE_SIZE;

    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        self.mutex
            .lock(|cell| cell.borrow_mut().write(offset, bytes))
    }

    fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        self.mutex.lock(|cell| cell.borrow_mut().erase(from, to))
    }
}
