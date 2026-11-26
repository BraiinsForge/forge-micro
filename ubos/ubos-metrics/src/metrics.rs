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

//! The metric a task pushes, its labels and the line budget of one
//! series. The store that receives a push is re-exported from here, so a
//! caller names one module.
//!
//! Endpoint model: `../../../docs/METRICS.md`.

use core::fmt::Write as _;

use embassy_time::{Duration, Instant};
#[cfg(feature = "metrics")]
use portable_atomic::Ordering;

use crate::consts::Metrics;
use crate::freshness::Expiry;
use crate::name::{Name, Unit};
#[cfg(feature = "metrics")]
use crate::store::{DROPPED, METRICS};
use crate::value::Value;

#[cfg(feature = "metrics")]
pub use crate::scrape::PrometheusMetricsService;
#[cfg(feature = "metrics")]
pub use crate::store::{MetricsStatics, run_ingest};

/// Bytes a series key may take, the name with its labels expanded. The
/// store asserts it against the longest key any metric renders, and each
/// byte costs one byte per map slot, so a board pays it `MAX_METRICS`
/// times over.
///
/// The budget is beside the metric because a board sizes its render
/// buffer from it in every build, and the store is absent from some.
pub(crate) const KEY_CAPACITY: usize = 80;

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

/// Bytes the text of one label value may take. The longest value a push
/// site writes is a short word or an index, and each byte costs one byte
/// for every label of every queued metric.
const LABEL_VALUE_CAPACITY: usize = 24;

// Only the store reads a label, and a build without it has no reader.
#[cfg_attr(not(feature = "metrics"), allow(dead_code))]
pub(crate) struct Label {
    pub(crate) name: &'static str,
    pub(crate) value: heapless::String<LABEL_VALUE_CAPACITY>,
}

/// Fixed-capacity label vector carried by a [`Metric`].
type Labels = heapless::Vec<Label, { Metrics::MAX_LABELS }>;

/// A single metric observation to push into the exposition channel.
///
/// Fields are private; construct through the `metric!` macro, which
/// checks the name and stamps the metric with the current monotonic
/// time, add each label with [`Metric::label`], and push it with
/// [`Metric::push`] or [`Metric::try_push`].
///
/// A measured metric stores the time of its measurement in **monotonic
/// milliseconds since boot** (`embassy_time::Instant::now().as_millis()`)
/// and a TTL. The exposition path adds the NTP epoch offset at scrape
/// time to render an absolute Unix-epoch timestamp. A constant stores
/// neither and renders with no timestamp.
pub struct Metric {
    name: &'static str,
    labels: Labels,
    value: Value,
    expiry: Option<Expiry>,
}

/// How many pushes [`Metric::try_push`] has dropped since boot. It is
/// always 0 in a build without the `metrics` feature, which has no
/// channel to fill.
#[must_use]
pub fn dropped() -> u32 {
    #[cfg(feature = "metrics")]
    return DROPPED.load(Ordering::Relaxed);
    #[cfg(not(feature = "metrics"))]
    0
}

impl Metric {
    /// Build a metric with no labels under a checked name, stamped with
    /// the current monotonic time.
    ///
    /// Push sites reach it through the `metric!` macro, which builds the
    /// name in a `const` context so that the check runs in the build.
    #[must_use]
    pub fn new<U: Unit>(name: Name<U>, value: U) -> Self {
        Self::stamped(name.as_str(), value.into())
    }

    /// Add the label `name` with the text of `value`.
    ///
    /// The store sorts the labels of a series, so the order of the calls
    /// does not change the series. Only the label capacity makes it
    /// matter, because the labels beyond it are dropped, the last first.
    /// A value that does not fit the capacity of a label value is not
    /// stored: the label keeps the pieces of the text that fitted, and a
    /// single text that is too long leaves it empty.
    #[must_use]
    pub fn label(mut self, name: &'static str, value: impl core::fmt::Display) -> Self {
        self.push_label(name, &value);
        self
    }

    /// Write `value` into a new label `name`. The value is a trait object
    /// so that the code is compiled once and not once for each type a
    /// push site labels with.
    fn push_label(&mut self, name: &'static str, value: &dyn core::fmt::Display) {
        let mut text = heapless::String::new();
        let _ = write!(text, "{value}");
        let _ = self.labels.push(Label { name, value: text });
    }

    /// Build a metric from a value that is already converted, stamped with
    /// the current monotonic time. The public constructor ends here, so
    /// the code is compiled once and not once for each unit pushed.
    fn stamped(name: &'static str, value: Value) -> Self {
        Self {
            name,
            labels: Labels::new(),
            value,
            expiry: Some(Expiry::new(
                Instant::now().as_millis(),
                Metrics::DEFAULT_TTL,
            )),
        }
    }

    /// Make the metric a measurement that expires `ttl` after now, rounded
    /// up to whole seconds. A publisher states its push interval times
    /// [`Metrics::TTL_INTERVALS`], so a series that stops leaves the
    /// exposition. The last of [`Metric::ttl`] and [`Metric::constant`]
    /// wins.
    #[must_use]
    pub fn ttl(mut self, ttl: Duration) -> Self {
        self.expiry = Some(Expiry::new(Instant::now().as_millis(), ttl));
        self
    }

    /// Make the metric a constant: it never expires and renders with no
    /// timestamp. Push it with [`Metric::push`], so it is never lost. The
    /// last of [`Metric::ttl`] and [`Metric::constant`] wins.
    #[must_use]
    pub fn constant(mut self) -> Self {
        self.expiry = None;
        self
    }

    /// Awaits while the channel is full. A build without the `metrics`
    /// feature has no channel, so the metric is dropped and the call
    /// returns at once.
    // The function stays `async` in a build without the feature, where it
    // awaits nothing, so that no push site depends on the feature.
    #[allow(clippy::unused_async)]
    pub async fn push(self) {
        #[cfg(feature = "metrics")]
        METRICS.send(self).await;
    }

    /// Drops the metric when the channel is full and counts the drop in
    /// [`dropped`], for a caller that must keep making progress while the
    /// ingest task cannot run. A ticker replaces a dropped sample with
    /// the next one. A driver that pushes a reading when it takes it
    /// keeps the older value until the next reading. A build without the
    /// `metrics` feature has no channel and drops every metric without a
    /// count.
    pub fn try_push(self) {
        #[cfg(feature = "metrics")]
        if METRICS.try_send(self).is_err() {
            DROPPED.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[must_use]
    pub fn name(&self) -> &'static str {
        self.name
    }

    #[must_use]
    pub fn value(&self) -> Value {
        self.value
    }

    /// When the metric was measured and how long it stays valid, or
    /// `None` for a constant.
    // Only the store reads this, and a build without it has no reader.
    #[cfg_attr(not(feature = "metrics"), allow(dead_code))]
    pub(crate) fn expiry(&self) -> Option<Expiry> {
        self.expiry
    }

    #[must_use]
    // Only the store reads this, and a build without it has no reader.
    #[cfg_attr(not(feature = "metrics"), allow(dead_code))]
    pub(crate) fn labels(&self) -> &[Label] {
        &self.labels
    }
}
