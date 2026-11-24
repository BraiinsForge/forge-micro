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

//! The metrics store: the channel that carries a pushed metric, the map
//! of current series that the ingest task fills from it, and the key and
//! the line budget of one series. It is one module so that one build
//! switch can remove the store as a whole.

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::mutex::Mutex;
use embassy_time::Instant;
use ubos_logging::warn;

use ubos_core::units::Temperature;

use crate::catalog;
use crate::consts::Metrics;
use crate::freshness::Expiry;
use crate::metrics::Metric;
use crate::name::Name;
use crate::value::Value;

#[cfg(test)]
#[path = "tests/store.rs"]
mod tests;

/// Stored value associated with a map key.
pub(crate) struct StoredValue {
    pub(crate) value: Value,
    pub(crate) expiry: Option<Expiry>,
}

/// Canonical map key: metric name plus labels sorted by name and serialized.
///
/// Labels are sorted so that `{a="1",b="2"}` and `{b="2",a="1"}` map to
/// the same key.
///
/// Budget: the longest key any metric renders is
/// [`LONGEST_RENDERED_KEY_BYTES`], and the capacity is checked against it
/// below rather than against a length restated here. A prose budget is
/// what went stale last time: it named a 49-byte key while a longer one
/// was already being emitted, and the two-label per-chain series that
/// followed overran the cap and was dropped at runtime for a `warn!`.
///
/// Each byte costs one byte per map slot, so a board pays it
/// `MAX_METRICS` times over and this is raised deliberately rather than
/// rounded up. A metric longer than the constant below must move both,
/// and the test beside it is what catches one that does not.
const KEY_CAPACITY: usize = 80;

/// The name of the longest key the tree renders, the summary of the chip
/// temperatures of a hashboard.
const LONGEST_KEY_NAME: Name<Temperature> = catalog::CHIP_TEMPERATURE_SUMMARY;

/// The labels of the longest key, in the sorted form that
/// [`MetricKey::from_metric`] builds. A hashboard index is one digit for
/// every board below ten hashboards, and `mean` is the longest statistic.
const LONGEST_KEY_LABELS: &str = "{hashboard=\"0\",stat=\"mean\"}";

/// Bytes of the longest key the tree renders. The name comes from the
/// catalog, so a rename moves this length with it.
const LONGEST_RENDERED_KEY_BYTES: usize =
    LONGEST_KEY_NAME.as_str().len() + LONGEST_KEY_LABELS.len();

const _: () = assert!(
    LONGEST_RENDERED_KEY_BYTES <= KEY_CAPACITY,
    "KEY_CAPACITY is below the longest key a metric renders"
);

/// Characters of the widest number a line carries, the digits of
/// `u64::MAX`. The text of every [`Value`] fits it, and a test of the
/// value holds that.
pub(crate) const MAX_INTEGER_CHARS: usize = u64::MAX.ilog10() as usize + 1;

/// Bytes the longest series line takes in a scrape: the key, a space, the
/// value, a space, the millisecond timestamp, and the newline. A
/// board sizes its render buffer as this times its series cap, so the
/// buffer holds a full map.
pub const METRIC_LINE_BYTES: usize =
    KEY_CAPACITY + 1 + MAX_INTEGER_CHARS + 1 + MAX_INTEGER_CHARS + 1;

#[derive(PartialEq, Eq, Hash)]
pub(crate) struct MetricKey(pub(crate) heapless::String<KEY_CAPACITY>);

impl MetricKey {
    /// Build a key from a metric, sorting its labels by name.
    fn from_metric(metric: &Metric) -> Option<Self> {
        let labels = metric.labels();
        let mut sorted_indices: [usize; Metrics::MAX_LABELS] = [0; Metrics::MAX_LABELS];
        let count = labels.len();
        for (index, slot) in sorted_indices.iter_mut().enumerate().take(count) {
            *slot = index;
        }
        // Insertion sort is fine for the bounded label count.
        for outer in 1..count {
            let pivot = sorted_indices[outer];
            let mut inner = outer;
            while inner > 0 && labels[sorted_indices[inner - 1]].name > labels[pivot].name {
                sorted_indices[inner] = sorted_indices[inner - 1];
                inner -= 1;
            }
            sorted_indices[inner] = pivot;
        }

        let mut key = heapless::String::<KEY_CAPACITY>::new();
        key.push_str(metric.name()).ok()?;
        if count > 0 {
            key.push('{').ok()?;
            for (position, &slot) in sorted_indices[..count].iter().enumerate() {
                if position > 0 {
                    key.push(',').ok()?;
                }
                key.push_str(labels[slot].name).ok()?;
                key.push_str("=\"").ok()?;
                push_escaped_label_value(&mut key, labels[slot].value.as_str())?;
                key.push('"').ok()?;
            }
            key.push('}').ok()?;
        }
        Some(MetricKey(key))
    }
}

/// Append a Prometheus-escaped label value to `key`. Returns `None` if the
/// buffer fills.
fn push_escaped_label_value(key: &mut heapless::String<KEY_CAPACITY>, value: &str) -> Option<()> {
    for character in value.chars() {
        match character {
            '\\' => key.push_str("\\\\").ok()?,
            '"' => key.push_str("\\\"").ok()?,
            '\n' => key.push_str("\\n").ok()?,
            other => key.push(other).ok()?,
        }
    }
    Some(())
}

/// Shared channel through which any task can push metrics.
pub(crate) static METRICS: Channel<CriticalSectionRawMutex, Metric, { Metrics::CHANNEL_DEPTH }> =
    Channel::new();

/// The exposition map and its render buffer, sized by the board.
///
/// `MAX` is the number of distinct metric series the map holds and must
/// be a power of two, the heapless `FnvIndexMap` invariant. The map is
/// keyed by the fully label-expanded series, so per-CPU and per-LM75A
/// label combinations each consume one slot; a board with three chains
/// and three pools pushes about 90. `BODY` is the exposition body
/// buffer, which has to hold every rendered series at once.
///
/// Both are a board's RAM budget rather than a property of this crate,
/// so a board states them in its own `config.rs` and
/// `ubos_firmware::serve_all!` declares the static from there. See the
/// Footprint section of `docs/STRUCTURE.md` for the per-board snapshot.
///
/// The two live in one static so a board places them together, hoisted
/// out of the task futures that would otherwise carry them.
pub struct MetricsStatics<const MAX: usize, const BODY: usize> {
    /// Current metric values, shared between [`run_ingest`] (the sole
    /// writer) and the exposition workers (readers).
    pub(crate) map: Mutex<
        CriticalSectionRawMutex,
        heapless::index_map::FnvIndexMap<MetricKey, StoredValue, MAX>,
    >,
    /// Exposition render buffer.
    ///
    /// A single `Mutex` shared across all exposition workers: with a
    /// pool depth above one, concurrent scrapes serialize on this lock
    /// while rendering. `serve_metrics` drops the map guard before the
    /// socket write, so a slow scrape blocks only other scrapes, never
    /// the ingestion task.
    pub(crate) body: Mutex<CriticalSectionRawMutex, [u8; BODY]>,
}

impl<const MAX: usize, const BODY: usize> MetricsStatics<MAX, BODY> {
    /// An empty map and a zeroed render buffer.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            map: Mutex::new(heapless::index_map::FnvIndexMap::new()),
            body: Mutex::new([0; BODY]),
        }
    }
}

impl<const MAX: usize, const BODY: usize> Default for MetricsStatics<MAX, BODY> {
    fn default() -> Self {
        Self::new()
    }
}

/// Declare one [`MetricsStatics`] static.
#[macro_export]
macro_rules! metrics_statics {
    ($name:ident, $max:expr, $body:expr) => {
        static $name: $crate::metrics::MetricsStatics<{ $max }, { $body }> =
            $crate::metrics::MetricsStatics::new();
    };
}

/// Consume pushed metrics into the shared map.
///
/// The sole writer of `statics`' map. `ubos_firmware::serve_all!` wraps
/// this in the board's `#[embassy_executor::task]`; the exposition
/// workers it spawns beside that task read the map through
/// [`PrometheusMetricsService`], so the exposition always reflects the
/// latest pushed values without a drain step on the serve path.
///
/// [`PrometheusMetricsService`]: crate::metrics::PrometheusMetricsService
pub async fn run_ingest<const MAX: usize, const BODY: usize>(
    statics: &'static MetricsStatics<MAX, BODY>,
) -> ! {
    loop {
        let metric = METRICS.receive().await;
        let mut map = statics.map.lock().await;
        upsert_metric(&mut map, &metric);
    }
}

/// Upsert a metric into the map, sorting its labels into a canonical key.
/// A full map first drops the expired series, so a series whose publisher
/// stopped does not keep a slot that a live series needs.
fn upsert_metric<const MAX: usize>(
    map: &mut heapless::index_map::FnvIndexMap<MetricKey, StoredValue, MAX>,
    metric: &Metric,
) {
    let Some(key) = MetricKey::from_metric(metric) else {
        warn!(
            "metrics key encoding overflowed for {}; dropping metric (raise KEY_CAPACITY)",
            metric.name()
        );
        return;
    };
    let stored = StoredValue {
        value: metric.value(),
        expiry: metric.expiry(),
    };
    let Err((key, stored)) = map.insert(key, stored) else {
        return;
    };
    let now_ms = Instant::now().as_millis();
    map.retain(|_, held| !held.expiry.is_some_and(|expiry| expiry.is_expired(now_ms)));
    if map.insert(key, stored).is_err() {
        warn!("metrics map full, dropping metric");
    }
}
