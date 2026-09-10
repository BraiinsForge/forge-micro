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

//! The chain's chips as the board's thermometer.
//!
//! A poll-family chain records each chip's die temperature and core
//! voltage into a [`ChainPvt`] as the serial actor's status turns come
//! round. This sensor reads that store for the thermal loop: it hands
//! the loop the hottest die the chain has reported, and once in a while
//! logs the chain's summary and publishes its core voltage, so no task
//! exists for that alone.
//!
//! Freshness is the chain's, not the chip's. A chip's own turn comes
//! round once per pass, which on a long chain is minutes, so judging
//! each chip against a clock either halts a healthy board or needs a
//! window too wide to mean anything. What the loop must not miss is the
//! chain going quiet altogether, and that is what this measures. The
//! cost is that one chip falling silent while its neighbours answer
//! leaves its last reading standing.

use crate::{Sample, Source, TemperatureSensor};
use embassy_futures::poll_once;
use embassy_time::{Duration, Instant, Ticker};
use ubos_asic::ChainPvt;
use ubos_core::units::Temperature;
use ubos_logging::{debug, warn};
use ubos_metrics::metrics::{Label, LabelValue, Labels, Metric, Value, push};

#[cfg(test)]
#[path = "tests/chain.rs"]
mod tests;

/// Longest the chain may go without a reading before it is silent.
///
/// Has to outlast the longest stretch in which nothing drives the chain,
/// which is not a missed poll but the walk from a bring-up reading to the
/// end of the tuner's frequency ramp, measured at 36 s.
pub const MAX_SILENCE: Duration = Duration::from_secs(60);

/// How often the chain's summary is logged and its voltage published.
const REPORT_INTERVAL: Duration = Duration::from_secs(10);

/// The chain's core voltage as the chips report it.
const CHAIN_VOLTAGE_METRIC: &str = "hashboard_chain_core_voltage_microvolts";

/// The spread of die temperatures across the chain, one series per
/// statistic under the `stat` label. The thermal loop already publishes
/// the hottest die as the sample it took, so this is here for the
/// spread: whether one die is running away from the rest, which a
/// single reading cannot say.
const CHAIN_TEMPERATURE_METRIC: &str = "hashboard_chain_chip_temperature_millicelsius";

/// The thermal loop's view of a chain whose chips are its thermometer.
pub struct ChainTemperature<const CHIPS: usize> {
    pvt: &'static ChainPvt<CHIPS>,
    /// Where silence is measured from until something answers, so a
    /// chain that takes its time enumerating does not arrive already
    /// expired.
    started: Instant,
    seen_reading: bool,
    report_ticker: Ticker,
    /// The chain's last answer as the previous report saw it, so a report
    /// with nothing new to say is recognised without a second clock.
    reported_answer: Option<Instant>,
}

impl<const CHIPS: usize> ChainTemperature<CHIPS> {
    #[must_use]
    pub fn new(pvt: &'static ChainPvt<CHIPS>) -> Self {
        Self {
            pvt,
            started: Instant::now(),
            seen_reading: false,
            report_ticker: Ticker::every(REPORT_INTERVAL),
            reported_answer: None,
        }
    }

    /// Whether nothing at all has arrived from the chain for
    /// [`MAX_SILENCE`].
    fn chain_is_silent(&self, now: Instant) -> bool {
        let last = self.pvt.last_answer().unwrap_or(self.started);
        now.saturating_duration_since(last) >= MAX_SILENCE
    }

    /// Every [`REPORT_INTERVAL`]: the chain's core voltage as a gauge and
    /// its summary as one log line. The summary carries the spread rather
    /// than an average, because the chips sit in series and the string
    /// can hold its total while sharing it badly.
    async fn report(&mut self) {
        // Polled rather than awaited: `sample` must not block, and the
        // tick is not lost when the future is dropped.
        if poll_once(self.report_ticker.next()).is_pending() {
            return;
        }
        // A late report takes its lateness instead of catching up: the
        // next deadline is a full interval from now, so a sample loop
        // that stalled cannot make this fire on several polls in a row
        // to make up the missed ones.
        self.report_ticker.reset();
        let answered = self.pvt.last_answer();
        if answered == self.reported_answer {
            warn!("chain pvt: nothing reported since the last report");
            return;
        }
        self.reported_answer = answered;
        if let Some(temperature) = self.pvt.temperature_summary() {
            let mean = temperature.total_millicelsius / i32::from(temperature.chips);
            Self::publish_stat("lowest", temperature.lowest_millicelsius).await;
            Self::publish_stat("mean", mean).await;
            Self::publish_stat("highest", temperature.highest_millicelsius).await;
            debug!(
                "chain pvt: {} chips report {} to {} mC, mean {} mC",
                temperature.chips,
                temperature.lowest_millicelsius,
                temperature.highest_millicelsius,
                mean
            );
        }
        let Some(voltage) = self.pvt.voltage_summary() else {
            warn!("chain pvt: no chip has reported a voltage");
            return;
        };
        push(Metric::with_no_labels(
            CHAIN_VOLTAGE_METRIC,
            Value::Int(i64::from(voltage.total_microvolts)),
        ))
        .await;
        debug!(
            "chain pvt: string {} mV, chips {} to {} mV",
            voltage.total_microvolts / 1000,
            voltage.lowest_microvolts / 1000,
            voltage.highest_microvolts / 1000
        );
    }

    /// One die-temperature statistic, tagged with the `stat` label so the
    /// three share a metric name.
    async fn publish_stat(stat: &'static str, millicelsius: i32) {
        let mut labels = Labels::new();
        let _ = labels.push(Label {
            name: "stat",
            value: LabelValue::Static(stat),
        });
        push(Metric::with_labels(
            CHAIN_TEMPERATURE_METRIC,
            labels,
            Value::Int(i64::from(millicelsius)),
        ))
        .await;
    }
}

impl<const CHIPS: usize> TemperatureSensor for ChainTemperature<CHIPS> {
    async fn sample(&mut self) -> Option<Sample> {
        self.report().await;
        if self.chain_is_silent(Instant::now()) {
            return None;
        }
        let reading = self
            .pvt
            .hottest_millicelsius()
            .map(Temperature::from_millicelsius)?;
        self.seen_reading = true;
        // The chips are the board's thermometer, so the same reading
        // gates the halt and drives the fans; it comes from the die
        // itself, so it is gated against the chip limit.
        Some(Sample {
            safety: reading,
            safety_source: Source::Chip,
            pid: None,
        })
    }

    /// The thermometer sits inside the chips the board is bringing up,
    /// so a missing reading is the normal state until the chain answers.
    /// Arming the halt from boot would stop mining before a temperature
    /// could exist, so the interlock arms on the first reading; a miss
    /// after that is a chain that reported and then fell silent.
    fn halts_on_fault(&self) -> bool {
        self.seen_reading
    }
}
