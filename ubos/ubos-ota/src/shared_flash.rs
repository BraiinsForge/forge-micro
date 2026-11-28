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

//! Shared blocking-flash adapter over one flash driver.
//!
//! A board has one flash peripheral, but two independent consumers need it:
//! the `ubos-config` record store and the OTA firmware target. Both must go
//! through the same driver instance, because a flash erase or program is a
//! critical section that two independent instances over the one peripheral
//! would race.
//!
//! [`SharedFlash`] resolves this by holding the one driver in a `'static`
//! [`Mutex`] of a [`RefCell`] and exposing a `Copy` handle that implements the
//! synchronous [`embedded_storage`] `NorFlash` traits. The config backend
//! takes this handle through [`ubos_config::SyncFlash`]; the OTA target takes
//! another. The mutex is locked only for the span of one synchronous flash
//! operation, never across an `await`. The [`CriticalSectionRawMutex`] makes
//! the handle sound against both another core and interrupt context.
//!
//! The type is generic over the flash driver `F`, so each MCU family names its
//! own driver, and over a [`FlashGuard`] run around each write and erase. A
//! family that must quiesce other work before a flash write supplies a guard;
//! [`NoGuard`] runs the operation directly. The RP family's guard is
//! `ubos_rp::ota::RpGuard`, built by `ubos_rp::ota::shared_blocking_flash`, and parks the core-1 serial worker.

use core::cell::RefCell;

use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embedded_storage::nor_flash::{ErrorType, NorFlash, ReadNorFlash};

/// Type of the `'static` mutex guarding the one blocking flash driver `F`.
///
/// Exposed so `main` can declare the backing `static` and so an OTA updater
/// can build `BlockingPartition`s over the very same mutex.
pub type SharedFlashMutex<F> = Mutex<CriticalSectionRawMutex, RefCell<F>>;

/// Hook run around each blocking write and erase.
///
/// A family whose flash writes must not race other work brackets each
/// operation with this. `E` is the flash driver's error type, so a guard that
/// gives up reports the failure as a flash error the caller already handles.
pub trait FlashGuard<E> {
    /// Runs `op`, optionally bracketing it with setup and teardown.
    ///
    /// # Errors
    ///
    /// Returns whatever `op` returns; a guard that cannot obtain the
    /// bracket it needs reports that as `E`, the flash driver's own error
    /// type, rather than running `op` unguarded.
    fn guard<R>(&self, op: impl FnOnce() -> Result<R, E>) -> Result<R, E>;
}

/// The default guard, which runs the operation directly.
#[derive(Clone, Copy)]
pub struct NoGuard;

impl<E> FlashGuard<E> for NoGuard {
    fn guard<R>(&self, op: impl FnOnce() -> Result<R, E>) -> Result<R, E> {
        op()
    }
}

/// `Copy` handle to the shared blocking flash driver `F`, guarded by `G`.
///
/// Implements the synchronous [`embedded_storage`] `NorFlash` traits by
/// locking the mutex for the span of each operation and delegating to `F`.
/// Write and erase run through [`FlashGuard`]; read does not. With a `Copy`
/// guard it is cheap to copy, so the config backend and any other consumer
/// can each hold one.
pub struct SharedFlash<F, G = NoGuard>
where
    F: NorFlash + 'static,
    G: FlashGuard<F::Error>,
{
    mutex: &'static SharedFlashMutex<F>,
    guard: G,
}

impl<F> SharedFlash<F>
where
    F: NorFlash + 'static,
{
    /// Wraps the `'static` flash mutex in a fresh `Copy` handle that runs
    /// each write and erase directly.
    #[must_use]
    pub const fn new(mutex: &'static SharedFlashMutex<F>) -> Self {
        Self::with_guard(mutex, NoGuard)
    }
}

impl<F, G> SharedFlash<F, G>
where
    F: NorFlash + 'static,
    G: FlashGuard<F::Error>,
{
    /// Wraps the `'static` flash mutex in a fresh handle that runs `guard`
    /// around each write and erase. The handle is `Copy` when `guard` is.
    #[must_use]
    pub const fn with_guard(mutex: &'static SharedFlashMutex<F>, guard: G) -> Self {
        Self { mutex, guard }
    }
}

impl<F, G> Clone for SharedFlash<F, G>
where
    F: NorFlash + 'static,
    G: FlashGuard<F::Error> + Copy,
{
    fn clone(&self) -> Self {
        *self
    }
}

impl<F, G> Copy for SharedFlash<F, G>
where
    F: NorFlash + 'static,
    G: FlashGuard<F::Error> + Copy,
{
}

impl<F, G> ErrorType for SharedFlash<F, G>
where
    F: NorFlash + 'static,
    G: FlashGuard<F::Error>,
{
    type Error = F::Error;
}

impl<F, G> ReadNorFlash for SharedFlash<F, G>
where
    F: NorFlash + 'static,
    G: FlashGuard<F::Error>,
{
    const READ_SIZE: usize = <F as ReadNorFlash>::READ_SIZE;

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        self.mutex
            .lock(|cell| cell.borrow_mut().read(offset, bytes))
    }

    fn capacity(&self) -> usize {
        self.mutex.lock(|cell| cell.borrow().capacity())
    }
}

impl<F, G> NorFlash for SharedFlash<F, G>
where
    F: NorFlash + 'static,
    G: FlashGuard<F::Error>,
{
    const WRITE_SIZE: usize = <F as NorFlash>::WRITE_SIZE;
    const ERASE_SIZE: usize = <F as NorFlash>::ERASE_SIZE;

    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        self.guard.guard(|| {
            self.mutex
                .lock(|cell| cell.borrow_mut().write(offset, bytes))
        })
    }

    fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        self.guard
            .guard(|| self.mutex.lock(|cell| cell.borrow_mut().erase(from, to)))
    }
}
