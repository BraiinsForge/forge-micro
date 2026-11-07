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

//! The chains' chips as the board's thermometer.
//!
//! A poll-family chain records each chip's die temperature and core
//! voltage into a [`ChainPvt`] as the serial actor's status turns come
//! round. This sensor reads one such store per chain for the thermal
//! loop: it hands the loop the hottest and the mean die temperature the
//! chains have reported, and once in a while logs each chain's summary
//! and publishes its core voltage, so no task exists for that alone.
//!
//! A board can own more than one chain, and a machine can have more
//! connectors than boards plugged into them, so the sensor reads only
//! the chains the miner found present. An absent or faulted chain has no
//! chip to run a PVT block at all, and counting it as a silent chain
//! would halt the board for hardware that is not there. Where no chain
//! is present at all the sensor answers `Idle`; see "Chain presence" in
//! `docs/MINING.md`. A present chain
//! that falls silent does not hold the sample back on the tick it goes
//! quiet, because a sibling chain may already hold the reading that
//! needs the trip; its own hottest die is cleared to `None` in its
//! `ChainStats` entry the same tick, which is how the dashboard sees it.
//! From the second silent sample on, the quiet chain's unread dies are
//! the greater risk, and the sensor answers nothing until the chain
//! speaks again, so the loop faults the way it does for one chain.
//!
//! Freshness is the chain's, not the chip's. A chip's own turn comes
//! round once per pass, which on a long chain is minutes, so judging
//! each chip against a clock either halts a healthy board or needs a
//! window too wide to mean anything. What the loop must not miss is a
//! chain going quiet altogether, and that is what this measures. The
//! cost is that one chip falling silent while its neighbours answer
//! leaves its last reading standing.

use core::fmt::Write as _;

use crate::metrics::publish_chip_reading;
use crate::{Accumulator, Reading, TempSample, TemperatureSensor};
use embassy_futures::poll_once;
use embassy_time::{Duration, Instant, Ticker};
use ubos_asic::{ChainPvt, ChipIndex};
use ubos_core::units::{Temperature, Voltage};
use ubos_logging::{debug, warn};
use ubos_metrics::chain::ChainStatsHandle;
use ubos_metrics::metrics::{Label, LabelValue, Labels, Metric, Value};

#[cfg(test)]
#[path = "tests/chain.rs"]
mod tests;

/// Longest a chain may go without a reading before it is silent.
///
/// Has to outlast the longest stretch in which nothing drives the chain,
/// which is not a missed poll but the walk from a bring-up reading to the
/// end of the tuner's frequency ramp, measured at 36 s.
pub const MAX_SILENCE: Duration = Duration::from_secs(60);

/// How many samples in a row a present chain may be silent before the
/// sensor stops answering, which is what faults the thermal loop. The
/// first silent sample still delivers a sibling chain's hot reading, so
/// a trip already in hand is not delayed; from the second on, the quiet
/// chain's unread dies count for more than the sibling's reading.
pub const SILENT_TICKS_BEFORE_FAULT: u8 = 2;

/// How often each chain's summary is logged and its voltage published.
const REPORT_INTERVAL: Duration = Duration::from_secs(10);

/// Microvolts in a millivolt, because the chain voltage series is in
/// microvolts.
const MICROVOLTS_PER_MILLIVOLT: i64 = 1000;

/// A chain's core voltage as its chips report it, one series per chain
/// under the `chain` label.
const CHAIN_VOLTAGE_METRIC: &str = "hashboard_chain_voltage_microvolts";

/// The spread of die temperatures across a chain, one series per
/// statistic under the `stat` label and per chain under the `chain` one.
/// Each die's reading is already published on its own, so this is here
/// for the spread: whether one die is running away from the rest, which a
/// single reading cannot say.
const CHAIN_TEMPERATURE_METRIC: &str = "hashboard_chain_chip_temperature_millicelsius";

/// The thermal loop's view of a board whose chips are its thermometer,
/// spanning every one of its `CHAINS` chains.
///
/// The reported values are the hottest and the mean chip across every
/// present chain, and each present chain's own hottest chip is written
/// into that chain's accounting, which is what the dashboard's per-chain
/// temperature column shows. The hottest die is the one field of that
/// accounting no other producer writes.
pub struct ChainTemperature<const CHIPS: usize, const CHAINS: usize> {
    pvt: &'static [ChainPvt<CHIPS>; CHAINS],
    /// Where each chain's presence is read and its hottest die is
    /// published. The board's table is sized to
    /// `ubos_machine::MAX_CHAINS` rather than to this board's own
    /// `CHAINS`, so [`Self::new`] asserts it holds enough slots to index
    /// by chain.
    stats: ChainStatsHandle,
    /// Where silence is measured from until something answers, so a
    /// chain that takes its time enumerating does not arrive already
    /// expired.
    started: Instant,
    report_ticker: Ticker,
    /// Each chain's last answer as the previous report saw it, so a
    /// report with nothing new to say is recognised without a second
    /// clock.
    reported_answer: [Option<Instant>; CHAINS],
    /// When each chain was last seen turning present, so silence is
    /// measured from there rather than from a pre-halt timestamp a stale
    /// `last_answer` would otherwise hold on to.
    became_present: [Option<Instant>; CHAINS],
    /// Each chain's presence as the previous sample saw it, so the edge
    /// into `Present` is caught to update `became_present`.
    was_present: [bool; CHAINS],
    /// Consecutive samples each present chain has been silent for, see
    /// [`SILENT_TICKS_BEFORE_FAULT`].
    silent_ticks: [u8; CHAINS],
}

impl<const CHIPS: usize, const CHAINS: usize> ChainTemperature<CHIPS, CHAINS> {
    /// # Panics
    ///
    /// Panics if `stats` holds fewer than `CHAINS` slots: the board's
    /// accounting table must be at least as long as the board's chain
    /// count for every chain to have a slot.
    #[must_use]
    pub fn new(pvt: &'static [ChainPvt<CHIPS>; CHAINS], stats: ChainStatsHandle) -> Self {
        assert!(
            stats.slots() >= CHAINS,
            "BUG: fewer ChainStats slots than the board's chains"
        );
        Self {
            pvt,
            stats,
            started: Instant::now(),
            report_ticker: Ticker::every(REPORT_INTERVAL),
            reported_answer: [None; CHAINS],
            became_present: [None; CHAINS],
            was_present: [false; CHAINS],
            silent_ticks: [0; CHAINS],
        }
    }

    /// Whether nothing at all has arrived from `chain` for
    /// [`MAX_SILENCE`], measured from whichever is more recent: the
    /// chain's last answer, or the moment it was last seen turning
    /// present. A chain that halted and came back present carries no
    /// answer from before the halt against it.
    fn chain_is_silent(&self, chain: usize, now: Instant) -> bool {
        let became_present = self.became_present[chain].unwrap_or(self.started);
        let last = self.pvt[chain]
            .last_answer()
            .unwrap_or(self.started)
            .max(became_present);
        now.saturating_duration_since(last) >= MAX_SILENCE
    }

    /// Whether a present chain has been silent for
    /// [`SILENT_TICKS_BEFORE_FAULT`] samples or more.
    fn stalled(&self) -> bool {
        self.silent_ticks
            .iter()
            .any(|&ticks| ticks >= SILENT_TICKS_BEFORE_FAULT)
    }

    /// Record `chain` turning present so [`Self::chain_is_silent`] does
    /// not measure it against a timestamp from before the halt.
    fn track_presence(&mut self, chain: usize, now: Instant, present: bool) {
        if present && !self.was_present[chain] {
            self.became_present[chain] = Some(now);
        }
        self.was_present[chain] = present;
    }

    /// Every [`REPORT_INTERVAL`]: each present chain's core voltage as a
    /// gauge and its summary as one log line. A summary carries the
    /// spread rather than an average, because the chips sit in series and
    /// the string can hold its total while sharing it badly.
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
        for chain in 0..CHAINS {
            // An absent chain has nothing to report and never will, so it
            // is passed over rather than warned about every interval.
            if self.stats.chain(chain).presence().is_present() {
                self.report_chain(chain).await;
            }
        }
    }

    /// One present chain's voltage gauge, die temperatures, their spread
    /// and log line.
    async fn report_chain(&mut self, chain: usize) {
        let answered = self.pvt[chain].last_answer();
        if answered == self.reported_answer[chain] {
            warn!(
                "chain {} pvt: nothing reported since the last report",
                chain
            );
            return;
        }
        self.reported_answer[chain] = answered;
        for chip in (0..=ChipIndex::MAX).take(CHIPS) {
            let Some(value) = self.pvt[chain].temperature(chip) else {
                continue;
            };
            publish_chip_reading(chain, chip, value).await;
        }
        if let Some(temperature) = self.pvt[chain].temperature_summary() {
            Self::publish_stat(chain, "lowest", temperature.lowest).await;
            Self::publish_stat(chain, "mean", temperature.mean).await;
            Self::publish_stat(chain, "highest", temperature.highest).await;
            debug!(
                "chain {} pvt: {} chips report {} to {} °C, mean {} °C",
                chain, temperature.chips, temperature.lowest, temperature.highest, temperature.mean
            );
        }
        let Some(voltage) = self.pvt[chain].voltage_summary() else {
            warn!("chain {} pvt: no chip has reported a voltage", chain);
            return;
        };
        Metric::indexed(
            CHAIN_VOLTAGE_METRIC,
            &[("chain", chain)],
            Value::Int(i64::from(voltage.total.mv()) * MICROVOLTS_PER_MILLIVOLT),
        )
        .push()
        .await;
        debug!(
            "chain {} pvt: string {} V, chips {} to {} V",
            chain, voltage.total, voltage.lowest, voltage.highest
        );
    }

    /// One chain's die-temperature statistic, tagged with the `stat`
    /// label so the three share a metric name.
    async fn publish_stat(chain: usize, stat: &'static str, temperature: Temperature) {
        let mut labels = chain_labels(chain);
        let _ = labels.push(Label {
            name: "stat",
            value: LabelValue::Static(stat),
        });
        Metric::with_labels(
            CHAIN_TEMPERATURE_METRIC,
            labels,
            Value::Int(i64::from(temperature.millicelsius())),
        )
        .push()
        .await;
    }
}

/// The `chain="<n>"` label, for the one metric that carries it alongside
/// a second label: the die-temperature spread also tags a `stat`, so it
/// cannot go through [`Metric::indexed`] alone. Since a board can own
/// more than one chain, the series would otherwise collide.
pub(crate) fn chain_labels(chain: usize) -> Labels {
    let mut chain_str = heapless::String::new();
    let _ = write!(chain_str, "{chain}");
    let mut labels = Labels::new();
    let _ = labels.push(Label {
        name: "chain",
        value: LabelValue::Short(chain_str),
    });
    labels
}

impl<const CHIPS: usize, const CHAINS: usize> TemperatureSensor
    for ChainTemperature<CHIPS, CHAINS>
{
    /// The lowest complete chip-voltage sum any present chain reports.
    ///
    /// The chains of one board share the rail this stands in for, so the
    /// reading of any one of them describes it, and the lowest is the
    /// worst-case rail reading: the board's droop shows on whichever
    /// chain sags most, and a higher reading from another chain would
    /// hide that. A partial sum would look like severe rail droop during
    /// bring-up, so a chain that has not reported every chip is skipped
    /// rather than summed.
    fn chain_voltage(&self) -> Option<Voltage> {
        let now = Instant::now();
        let mut lowest: Option<Voltage> = None;
        for chain in 0..CHAINS {
            if !self.stats.chain(chain).presence().is_present() || self.chain_is_silent(chain, now)
            {
                continue;
            }
            let Some(voltage) = self.pvt[chain].voltage_summary() else {
                continue;
            };
            if usize::from(voltage.chips) != CHIPS {
                continue;
            }
            lowest = Some(lowest.map_or(voltage.total, |held: Voltage| held.min(voltage.total)));
        }
        lowest
    }

    async fn sample(&mut self) -> Reading {
        self.report().await;
        let now = Instant::now();
        let mut chips: Option<Accumulator> = None;
        let mut any_present = false;
        for chain in 0..CHAINS {
            let present = self.stats.chain(chain).presence().is_present();
            self.track_presence(chain, now, present);
            if !present {
                // A chain that dropped out must not keep reporting the
                // last temperature it read while present.
                self.stats.chain(chain).set_hottest(None);
                self.silent_ticks[chain] = 0;
                continue;
            }
            any_present = true;
            let silent = self.chain_is_silent(chain, now);
            self.silent_ticks[chain] = if silent {
                self.silent_ticks[chain].saturating_add(1)
            } else {
                0
            };
            let summary = (!silent)
                .then(|| self.pvt[chain].temperature_summary())
                .flatten();
            self.stats
                .chain(chain)
                .set_hottest(summary.map(|summary| summary.highest.millicelsius()));
            if let Some(summary) = summary {
                chips = chips
                    .map(|acc| acc.include(summary))
                    .or_else(|| Some(summary.into()));
            }
        }
        // No chain is present, so no chip is powered: nothing here can
        // overheat and nothing owes a reading. Why that is an idle board
        // and not a fault: "Chain presence" in `docs/MINING.md`.
        if !any_present {
            return Reading::Idle;
        }
        // A present chain that has just fallen silent does not hold this
        // sample back: another chain may already hold the reading that
        // needs the trip. Once the quiet chain stays silent, its unread
        // dies are the greater risk, and the sensor stops answering so
        // the loop faults the way it does for one chain.
        if self.stalled() {
            return Reading::Unreadable;
        }
        // The chips are the board's thermometer, so they both gate the
        // halt and drive the fans; the reading comes from the die itself,
        // so it is gated against the chip limit.
        chips.map_or(Reading::Unreadable, |acc| {
            Reading::Measured(TempSample {
                chip: Some(acc.stats()),
                hashboard: None,
            })
        })
    }
}
