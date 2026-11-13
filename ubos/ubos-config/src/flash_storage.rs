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

//! Flash-backed [`Storage`] over a `sequential-storage` map.

use embedded_storage_async::nor_flash::NorFlash;
use sequential_storage::{
    cache::{Cache, Uncached},
    map::{MapConfig, MapStorage},
};

use crate::storage::Storage;

/// The widest flash word `sequential-storage` supports. The map reads an
/// item back rounded up to the flash word, so the scratch is rounded up
/// to the widest word a board can have.
const MAX_WORD_SIZE: usize = 32;

/// One map item: the `u8` key and the widest record, rounded up to the
/// flash word. Derived from [`crate::LOAD_BUFFER_SIZE`] so the encode
/// budget and the flash scratch cannot drift.
pub(crate) const SCRATCH_SIZE: usize =
    (size_of::<u8>() + crate::LOAD_BUFFER_SIZE).next_multiple_of(MAX_WORD_SIZE);

/// Flash-backed config storage confined to `range`; the board names the
/// range.
pub struct FlashStorage<F: NorFlash> {
    map: MapStorage<u8, F, Cache<Uncached, Uncached, Uncached, u8>>,
}

impl<F: NorFlash> FlashStorage<F> {
    /// `range` must be erase-aligned and span at least two erase pages;
    /// `MapConfig::new` rejects an invalid one at compile time.
    pub fn new(flash: F, range: core::ops::Range<u32>) -> Self {
        Self {
            map: MapStorage::new(flash, MapConfig::new(range), Cache::new_uncached()),
        }
    }
}

impl<F: NorFlash> Storage for FlashStorage<F> {
    type Error = sequential_storage::Error<F::Error>;

    async fn load<'b>(
        &mut self,
        key: u8,
        buffer: &'b mut [u8],
    ) -> Result<Option<&'b [u8]>, Self::Error> {
        let mut scratch = [0u8; SCRATCH_SIZE];
        let result = self.map.fetch_item::<&[u8]>(&mut scratch, &key).await?;
        let Some(stored) = result else {
            return Ok(None);
        };
        let length = stored.len().min(buffer.len());
        buffer[..length].copy_from_slice(&stored[..length]);
        Ok(Some(&buffer[..length]))
    }

    async fn store(&mut self, key: u8, data: &[u8]) -> Result<(), Self::Error> {
        let mut scratch = [0u8; SCRATCH_SIZE];
        self.map.store_item(&mut scratch, &key, &data).await
    }
}
