//! File-backed [`Storage`] implementation for host targets.
//!
//! Each key is stored as a separate file under a directory chosen by
//! the caller. The file name is `<key>.bin`. Open/read/close per call;
//! no in-process caching. The async trait methods contain no `.await`
//! points because `std::fs` is synchronous, but the `async fn` signature
//! satisfies the [`Storage`] trait without a wrapper.

#![cfg(feature = "std")]

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crate::storage::Storage;

/// File-backed config storage for host-runner and test environments.
///
/// One file per key, named `<key>.bin`, lives under the directory
/// provided to [`FileStorage::new`]. The directory is created on first
/// use if it does not exist.
pub struct FileStorage {
    dir: PathBuf,
}

impl FileStorage {
    /// Create a [`FileStorage`] rooted at `dir`.
    ///
    /// The directory need not exist yet; it is created lazily on the
    /// first [`Storage::store`] call.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// Return the path for the given key.
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
        // `Read::read` is allowed to return a short read; loop until the
        // buffer is full or EOF so a partial read never decodes as a
        // truncated blob.
        let mut total = 0usize;
        while total < buffer.len() {
            match file.read(&mut buffer[total..])? {
                0 => break,
                bytes_read => total += bytes_read,
            }
        }
        // Probe one extra byte: if the file is larger than the buffer,
        // surface an error so the caller logs `LoadError::Io` and falls
        // back to defaults instead of decoding a silently truncated blob.
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
