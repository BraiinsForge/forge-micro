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

//! The count of requests a listener served, which the listener exports as
//! the series `http_requests` with its port as a label. The listener
//! serves every request, so it is the one place that counts each once.

use core::convert::Infallible;

use embassy_time::{Duration, Ticker};
use portable_atomic::{AtomicBool, AtomicU32, Ordering};
use ubos_metrics::consts::Metrics;
use ubos_metrics::metric;
use ubos_metrics::value::Count;

#[cfg(test)]
#[path = "tests/requests.rs"]
mod tests;

/// Cadence at which the count is pushed.
const EXPORT_INTERVAL: Duration = Duration::from_secs(1);

/// The series leaves the exposition when its exporter stops, after the
/// pushes that [`Metrics::TTL_INTERVALS`] allows to miss.
const EXPORT_TTL: Duration =
    Duration::from_secs(EXPORT_INTERVAL.as_secs() * Metrics::TTL_INTERVALS as u64);

/// The requests that the workers of one listener served since boot.
///
/// A pool has one counter, shared by its workers. The first worker to
/// call [`RequestCounter::claim_export`] exports it.
pub struct RequestCounter {
    port: u16,
    count: AtomicU32,
    exporting: AtomicBool,
}

impl RequestCounter {
    /// A counter for the listener on `port`.
    #[must_use]
    pub const fn new(port: u16) -> Self {
        Self {
            port,
            count: AtomicU32::new(0),
            exporting: AtomicBool::new(false),
        }
    }

    pub(crate) fn record(&self) {
        self.count.fetch_add(1, Ordering::Relaxed);
    }

    /// True for the first caller and false for every later one, so one
    /// worker of the pool exports and the others only count.
    pub(crate) fn claim_export(&self) -> bool {
        !self.exporting.swap(true, Ordering::Relaxed)
    }

    /// Pushes the count as a state, once for each interval. A push that
    /// finds the channel full is dropped, and the next tick replaces it.
    pub(crate) async fn export(&self) -> Infallible {
        let mut ticker = Ticker::every(EXPORT_INTERVAL);
        loop {
            metric!(
                "http_requests",
                Count(u64::from(self.count.load(Ordering::Relaxed)))
            )
            .label("port", self.port)
            .state(EXPORT_TTL)
            .try_push();
            ticker.next().await;
        }
    }
}
