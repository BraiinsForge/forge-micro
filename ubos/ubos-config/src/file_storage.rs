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

//! File-backed [`Storage`] for host targets: one `<key>.bin` per key.

#![cfg(feature = "std")]

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crate::storage::Storage;

pub struct FileStorage {
    dir: PathBuf,
}

impl FileStorage {
    /// `dir` is created on the first store.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    fn key_path(&self, key: u8) -> PathBuf {
        self.dir.join(format!("{key}.bin"))
    }
}

impl Storage for FileStorage {
    type Error = std::io::Error;

    async fn load<'b>(
        &mut self,
        key: u8,
        buffer: &'b mut [u8],
    ) -> Result<Option<&'b [u8]>, Self::Error> {
        let path = self.key_path(key);
        if !Path::new(&path).exists() {
            return Ok(None);
        }
        let mut file = std::fs::File::open(&path)?;
        // A short read must not decode as a truncated blob.
        let mut total = 0usize;
        while total < buffer.len() {
            match file.read(&mut buffer[total..])? {
                0 => break,
                bytes_read => total += bytes_read,
            }
        }
        // A file larger than the buffer is an error, not a truncated blob.
        let mut overflow = [0u8; 1];
        if file.read(&mut overflow)? != 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "stored config blob exceeds load buffer",
            ));
        }
        Ok(Some(&buffer[..total]))
    }

    async fn store(&mut self, key: u8, data: &[u8]) -> Result<(), Self::Error> {
        std::fs::create_dir_all(&self.dir)?;
        let path = self.key_path(key);
        let mut file = std::fs::File::create(&path)?;
        file.write_all(data)
    }
}
