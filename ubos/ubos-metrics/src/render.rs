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

//! The render of the series map as a Prometheus text exposition into the
//! body buffer of the store. It is beside the store so that the service
//! that answers a scrape needs no view of the map.

use core::fmt::{self, Write as _};

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::MutexGuard;
use embassy_time::Instant;
use ubos_logging::warn;

use crate::freshness::Expiry;
use crate::store::{MetricKey, MetricsStatics, StoredValue};

#[cfg(test)]
#[path = "tests/render.rs"]
mod tests;

const MILLIS_PER_SECOND: u64 = 1_000;

/// A rendered exposition. It holds the body buffer of the store until it
/// drops, so a second scrape waits for the first to finish its write.
pub struct Exposition<'statics, const BODY: usize> {
    body: MutexGuard<'statics, CriticalSectionRawMutex, [u8; BODY]>,
    len: usize,
}

impl<const BODY: usize> Exposition<'_, BODY> {
    /// The text of the exposition.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.body[..self.len]
    }
}

impl<const MAX: usize, const BODY: usize> MetricsStatics<MAX, BODY> {
    /// Render every series that has not expired. `epoch_offset_secs` is
    /// the seconds from boot to the Unix epoch, or 0 while the clock is
    /// not set, and turns the time of a measurement into a Unix
    /// timestamp. The map lock is released before the render returns, so
    /// a slow write to the scraper never blocks ingestion.
    pub async fn render(&self, epoch_offset_secs: u64) -> Exposition<'_, BODY> {
        let mut body = self.body.lock().await;
        let len = {
            let map = self.map.lock().await;
            let mut writer = BodyWriter::new(&mut *body);
            // HELP and TYPE lines are optional, and the buffer is tight.
            render_stored(
                &mut writer,
                &map,
                epoch_offset_secs,
                Instant::now().as_millis(),
            );
            // A truncated exposition is malformed and the scraper rejects
            // it, so the overflow is told here, where its cause is known.
            if writer.overflowed {
                warn!(
                    "prometheus response buffer overflow; truncated exposition served \
                     (raise the board's METRICS_RESPONSE_BYTES or reduce series count)"
                );
            }
            writer.len
        };
        Exposition { body, len }
    }
}

/// A `fmt::Write` over a byte slice. A write past the end fails and
/// latches `overflowed`, and every later write fails too. `render_metric`
/// drops the line that overflowed, so the text ends on a whole line.
struct BodyWriter<'buffer> {
    buffer: &'buffer mut [u8],
    len: usize,
    overflowed: bool,
}

impl<'buffer> BodyWriter<'buffer> {
    fn new(buffer: &'buffer mut [u8]) -> Self {
        Self {
            buffer,
            len: 0,
            overflowed: false,
        }
    }
}

impl fmt::Write for BodyWriter<'_> {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        if self.overflowed {
            return Err(fmt::Error);
        }
        let end = self.len + text.len();
        let Some(slot) = self.buffer.get_mut(self.len..end) else {
            self.overflowed = true;
            return Err(fmt::Error);
        };
        slot.copy_from_slice(text.as_bytes());
        self.len = end;
        Ok(())
    }
}

/// Render every stored series that has not expired at `now_ms`, in
/// monotonic milliseconds. The render is where a series whose publisher
/// stopped pushing leaves the exposition. The ingest task removes expired
/// entries only when the map is full.
fn render_stored<const MAX: usize>(
    writer: &mut BodyWriter<'_>,
    map: &heapless::index_map::FnvIndexMap<MetricKey, StoredValue, MAX>,
    epoch_offset_secs: u64,
    now_ms: u64,
) {
    for (key, stored) in map {
        if stored
            .expiry
            .is_some_and(|expiry| expiry.is_expired(now_ms))
        {
            continue;
        }
        render_metric(writer, key.0.as_str(), stored, epoch_offset_secs);
    }
}

/// Render one series as one line. A stored time is monotonic
/// milliseconds since boot, and the epoch offset turns it into Unix
/// milliseconds. The line of a state or a constant, and every line while
/// the clock is not set, carries no timestamp, so the scraper stamps it.
fn render_metric(
    writer: &mut BodyWriter<'_>,
    key: &str,
    stored: &StoredValue,
    epoch_offset_secs: u64,
) {
    let start = writer.len;
    let _ = write!(writer, "{key} {}", stored.value);
    let stamp_ms = stored.expiry.and_then(Expiry::stamp_ms);
    if let (Some(at_ms), true) = (stamp_ms, epoch_offset_secs != 0) {
        let ts_ms = at_ms + epoch_offset_secs * MILLIS_PER_SECOND;
        let _ = write!(writer, " {ts_ms}");
    }
    let _ = writeln!(writer);
    if writer.overflowed {
        writer.len = start;
    }
}
