//! Adapter that presents a synchronous [`embedded_storage`] `NorFlash`
//! implementation as the async variant `sequential-storage` requires.
//!
//! Every flash driver in this workspace (`embassy-rp`'s `Blocking` flash,
//! `embassy-stm32`'s `Blocking` flash, `esp-storage`'s `FlashStorage`)
//! implements `embedded_storage::nor_flash::NorFlash` directly, while
//! the async variants either need extra wiring (DMA channels, IRQ
//! bindings) or do not exist at all. Erase and write are blocking on
//! every platform regardless of the trait surface, so the adapter
//! costs nothing at runtime.

use embedded_storage::nor_flash::{
    ErrorType as SyncErrorType, NorFlash as SyncNorFlash, ReadNorFlash as SyncReadNorFlash,
};
use embedded_storage_async::nor_flash::{
    MultiwriteNorFlash as AsyncMultiwriteNorFlash, NorFlash as AsyncNorFlash,
    ReadNorFlash as AsyncReadNorFlash,
};

/// Wraps a synchronous [`SyncNorFlash`] as an async [`AsyncNorFlash`].
///
/// The async methods on the wrapper await no futures; each one delegates
/// straight to the inner sync method. The blocking erase/write briefly
/// stalls the issuing executor, including mining on single-core boards,
/// which is no longer halted for the write. Config writes are infrequent
/// and each op is short, so the stall is acceptable.
pub struct SyncFlash<F>(pub F);

impl<F: SyncErrorType> SyncErrorType for SyncFlash<F> {
    type Error = F::Error;
}

impl<F: SyncReadNorFlash> AsyncReadNorFlash for SyncFlash<F> {
    const READ_SIZE: usize = F::READ_SIZE;

    async fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        self.0.read(offset, bytes)
    }

    fn capacity(&self) -> usize {
        self.0.capacity()
    }
}

impl<F: SyncNorFlash> AsyncNorFlash for SyncFlash<F> {
    const WRITE_SIZE: usize = F::WRITE_SIZE;
    const ERASE_SIZE: usize = F::ERASE_SIZE;

    async fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        self.0.erase(from, to)
    }

    async fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        self.0.write(offset, bytes)
    }
}

impl<F: embedded_storage::nor_flash::MultiwriteNorFlash> AsyncMultiwriteNorFlash for SyncFlash<F> {}
