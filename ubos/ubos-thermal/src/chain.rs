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

//! The chains' chips as the board's thermometer. The sensor reads each
//! present chain's [`ChainPvt`] store, hands the thermal loop the
//! hottest and the mean die across them, and every so often logs each
//! chain's summary and publishes its voltage, so no task exists for
//! that alone.
//!
//! Only present chains are read: an absent one has no chip to run a PVT
//! block, and counting it silent would halt the board for hardware that
//! is not there. With none present the sensor submits nothing; see "Chain
//! presence" in `docs/MINING.md`. A chain that has just gone quiet is
//! handled by [`SILENT_TICKS_BEFORE_FAULT`].
//!
//! Freshness is the chain's, not the chip's. A chip's turn comes round
//! once per pass, minutes on a long chain, so judging each chip against
//! a clock either halts a healthy board or needs a window too wide to
//! mean anything. The cost is that one chip falling silent among
//! answering neighbours leaves its last reading standing.
//!
//! The series of a chip is the driver's to move: it hands each die to
//! thermal when the chip answers. This sensor hands the filter of thermal
//! the hottest and the mean die of the chains on every sample, because a
//! chip that answers once in a pass would leave its window between two
//! turns.

use crate::chain_readings::ChainReadings;
use crate::{Accumulator, Handle};
use embassy_futures::poll_once;
use embassy_time::{Duration, Instant, Ticker};
use ubos_asic::ChainPvt;
use ubos_core::index::HashboardIndex;
use ubos_core::units::Voltage;
use ubos_logging::{debug, warn};
use ubos_metrics::catalog::HASHBOARD_LABEL;
use ubos_metrics::metric;
use ubos_temperature::ReadingSource;

#[cfg(test)]
#[path = "tests/chain.rs"]
mod tests;

/// Longest a chain may go without a reading before it is silent. It
/// must outlast the walk from a bring-up reading to the end of the
/// tuner's ramp, measured at 36 s, which is the longest stretch in which
/// nothing drives the chain.
pub const MAX_SILENCE: Duration = Duration::from_secs(60);

/// How many samples in a row a present chain may be silent before no
/// chain submits its dies, so the chip filter runs out and mining cuts
/// the rail. The first
/// silent sample still delivers a sibling's hot reading, so a trip
/// already in hand is not delayed; from the second on, the quiet chain's
/// unread dies count for more.
pub const SILENT_TICKS_BEFORE_FAULT: u8 = 2;

const REPORT_INTERVAL: Duration = Duration::from_secs(10);

/// Which chains the miner found present. The miner owns that fact and
/// depends on this crate, so the board hands its answer in through this
/// trait.
pub trait PresentChains {
    fn is_present(&self, chain: HashboardIndex) -> bool;
}

/// The thermal loop's view of a board whose chips are its thermometer.
/// Each present chain's own hottest chip and voltage sum are written
/// into [`ChainReadings`].
pub struct ChainTemperature<P: PresentChains, const CHIPS: usize, const CHAINS: usize> {
    pvt: &'static [ChainPvt<CHIPS>; CHAINS],
    presence: P,
    /// Sized to `ubos_machine::MAX_CHAINS`, not this board's `CHAINS`,
    /// so [`Self::new`] asserts it is long enough to index by chain.
    readings: ChainReadings,
    /// Where the dies of every sample are submitted, so thermal filters
    /// them.
    thermal: Handle,
    /// Silence is measured from here until something answers, so a slow
    /// enumeration does not arrive already expired.
    started: Instant,
    report_ticker: Ticker,
    /// The previous report's answer, so a report with nothing new is
    /// recognised without a second clock.
    reported_answer: [Option<Instant>; CHAINS],
    /// So silence is measured from here, not from a pre-halt timestamp
    /// a stale `last_answer` holds on to.
    became_present: [Option<Instant>; CHAINS],
    /// The previous sample's presence, so the edge into `Present` is
    /// caught.
    was_present: [bool; CHAINS],
    /// See [`SILENT_TICKS_BEFORE_FAULT`].
    silent_ticks: [u8; CHAINS],
}

impl<P: PresentChains, const CHIPS: usize, const CHAINS: usize> ChainTemperature<P, CHIPS, CHAINS> {
    /// # Panics
    ///
    /// Panics if the readings of `thermal` hold fewer than `CHAINS`
    /// slots: every chain of the board needs a slot.
    #[must_use]
    pub fn new(pvt: &'static [ChainPvt<CHIPS>; CHAINS], presence: P, thermal: Handle) -> Self {
        let readings = thermal.chains();
        assert!(
            readings.slots() >= CHAINS,
            "BUG: fewer chain reading slots than the board's chains"
        );
        Self {
            pvt,
            presence,
            readings,
            thermal,
            started: Instant::now(),
            report_ticker: Ticker::every(REPORT_INTERVAL),
            reported_answer: [None; CHAINS],
            became_present: [None; CHAINS],
            was_present: [false; CHAINS],
            silent_ticks: [0; CHAINS],
        }
    }

    /// Measured from whichever is later: the chain's last answer, or
    /// the moment it was last seen turning present, so a chain that
    /// halted and came back carries nothing from before against it.
    fn chain_is_silent(&self, chain: HashboardIndex, now: Instant) -> bool {
        let slot = chain.slot();
        let became_present = self.became_present[slot].unwrap_or(self.started);
        let last = self.pvt[slot]
            .last_answer()
            .unwrap_or(self.started)
            .max(became_present);
        now.saturating_duration_since(last) >= MAX_SILENCE
    }

    fn stalled(&self) -> bool {
        self.silent_ticks
            .iter()
            .any(|&ticks| ticks >= SILENT_TICKS_BEFORE_FAULT)
    }

    /// See [`Self::chain_is_silent`].
    fn track_presence(&mut self, chain: HashboardIndex, now: Instant, present: bool) {
        let slot = chain.slot();
        if present && !self.was_present[slot] {
            self.became_present[slot] = Some(now);
        }
        self.was_present[slot] = present;
    }

    /// Every [`REPORT_INTERVAL`], each present chain's voltage gauge and one
    /// summary line. The summary carries the spread, not an average: the chips
    /// sit in series, so the string can hold its total while sharing it badly.
    fn report(&mut self) {
        // Polled rather than awaited: `sample` must not block, and the
        // tick is not lost when the future is dropped.
        if poll_once(self.report_ticker.next()).is_pending() {
            return;
        }
        // The next deadline is a full interval from now, so a stalled
        // loop cannot make this fire repeatedly to catch up.
        self.report_ticker.reset();
        for chain in HashboardIndex::range(CHAINS) {
            if self.presence.is_present(chain) {
                self.report_chain(chain);
            }
        }
    }

    /// The sum of the core voltages of `chain`, or `None` when the chain
    /// is absent, silent, or has not reported every chip.
    fn voltage_of(&self, chain: HashboardIndex, now: Instant) -> Option<Voltage> {
        if !self.presence.is_present(chain) || self.chain_is_silent(chain, now) {
            return None;
        }
        let voltage = self.pvt[chain.slot()].voltage_summary()?;
        if usize::from(voltage.chips) != CHIPS {
            return None;
        }
        Some(voltage.total)
    }

    /// One present chain's voltage gauge and log lines.
    fn report_chain(&mut self, chain: HashboardIndex) {
        let slot = chain.slot();
        let answered = self.pvt[slot].last_answer();
        if answered == self.reported_answer[slot] {
            warn!(
                "chain {} pvt: nothing reported since the last report",
                chain
            );
            return;
        }
        self.reported_answer[slot] = answered;
        if let Some(temperature) = self.pvt[slot].temperature_summary() {
            debug!(
                "chain {} pvt: {} chips report {} to {} °C, mean {} °C",
                chain, temperature.chips, temperature.lowest, temperature.highest, temperature.mean
            );
        }
        let Some(voltage) = self.pvt[slot].voltage_summary() else {
            warn!("chain {} pvt: no chip has reported a voltage", chain);
            return;
        };
        // The sum of the chip voltages of the hashboard, as its chips
        // report them.
        metric!("chip_voltage_sum_volts", voltage.total)
            .label(HASHBOARD_LABEL, chain)
            .try_push();
        debug!(
            "chain {} pvt: string {} V, chips {} to {} V",
            chain, voltage.total, voltage.lowest, voltage.highest
        );
    }
}

impl<P: PresentChains, const CHIPS: usize, const CHAINS: usize> ReadingSource
    for ChainTemperature<P, CHIPS, CHAINS>
{
    async fn sample(&mut self) {
        self.report();
        let now = Instant::now();
        let mut chips: Option<Accumulator> = None;
        let mut any_present = false;
        for chain in HashboardIndex::range(CHAINS) {
            let slot = chain.slot();
            let present = self.presence.is_present(chain);
            self.track_presence(chain, now, present);
            if !present {
                self.readings.set_hottest(chain, None);
                self.readings.set_voltage(chain, None);
                self.silent_ticks[slot] = 0;
                continue;
            }
            any_present = true;
            let silent = self.chain_is_silent(chain, now);
            self.silent_ticks[slot] = if silent {
                self.silent_ticks[slot].saturating_add(1)
            } else {
                0
            };
            let summary = (!silent)
                .then(|| self.pvt[slot].temperature_summary())
                .flatten();
            self.readings
                .set_hottest(chain, summary.map(|summary| summary.highest));
            self.readings
                .set_voltage(chain, self.voltage_of(chain, now));
            if let Some(summary) = summary {
                chips = chips
                    .map(|acc| acc.include(summary))
                    .or_else(|| Some(summary.into()));
            }
        }
        // Nothing is powered, so nothing owes a reading; see the module
        // doc.
        if !any_present {
            return;
        }
        // See `SILENT_TICKS_BEFORE_FAULT`.
        if self.stalled() {
            return;
        }
        let Some(dies) = chips.map(Accumulator::stats) else {
            return;
        };
        // A stalled chain stops the dies of every chain, so the filter of
        // thermal empties and the readings of a sibling cannot stand in
        // for the dies that nothing reads.
        self.thermal.submit_dies(dies).await;
    }
}
