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

//! The tuner trait and the types it takes and returns.
//!
//! A tuner returns a [`Plan`] from the readings the loop passes it. The
//! loop owns the hardware, writes the plan and calls [`Tuner::step`]
//! again after the plan's delay. Readings a board reports beyond the
//! common ones are the tuner's [`Tuner::PlatformReadings`].

#![no_std]

use embassy_time::{Duration, Instant};
use ubos_core::units::{Frequency, Temperature, Voltage};
use ubos_psu::PsuReading;

/// Readings of one chain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChainReadings {
    /// Chips found on the chain at its most recent bring-up.
    pub chips: u16,
    /// The frequency last set on the chain.
    pub frequency: Frequency,
    /// The hottest die temperature reported, `None` before the first
    /// reading.
    pub hottest: Option<Temperature>,
    /// Smoothed hashrate in hashes per second.
    pub hashrate: u64,
}

/// Readings the loop passes to one step.
#[derive(Clone, Copy)]
pub struct Readings<const CHAINS: usize> {
    /// When the readings were taken.
    pub now: Instant,
    /// Rail voltage last applied by a successful command, independent of PSU samples.
    /// A sample can predate the last successful rail write, so its setpoint can be stale.
    pub voltage: Voltage,
    /// The last PSU sample, `None` before the first one.
    pub psu: Option<PsuReading>,
    /// The configured die temperature at which mining halts. `None` when no
    /// die temperature halts it.
    pub dangerous_chip_temp: Option<Temperature>,
    /// One entry per chain connector, `None` where no chain was found.
    pub chains: [Option<ChainReadings>; CHAINS],
}

/// Commands the loop writes after one step.
///
/// The loop writes the frequencies that decrease, then the rail, then
/// the frequencies that increase.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Plan<const CHAINS: usize> {
    /// Rail voltage to set, `None` to leave it.
    pub rail: Option<Voltage>,
    /// Frequency to set per chain, `None` to leave it. The loop ignores
    /// the entry of a connector with no chain.
    pub frequencies: [Option<Frequency>; CHAINS],
    /// How long the loop waits before the next step.
    pub delay: Duration,
}

/// The tuning algorithm of one machine.
///
/// # Example
///
/// A tuner that raises every present chain toward one frequency:
///
/// ```
/// use embassy_time::{Duration, Instant};
/// use ubos_core::units::{Frequency, Voltage};
/// use ubos_tuner::{ChainReadings, Plan, Readings, Tuner};
///
/// struct Raise {
///     increment: Frequency,
/// }
///
/// impl<const CHAINS: usize> Tuner<CHAINS> for Raise {
///     type Target = Frequency;
///     type PlatformReadings = ();
///
///     fn step(&mut self, target: Frequency, readings: &Readings<CHAINS>, _: &()) -> Plan<CHAINS> {
///         let frequencies = readings.chains.map(|chain| {
///             let chain = chain?;
///             let raised = chain.frequency.khz() + self.increment.khz();
///             let next = Frequency::from_khz(raised.min(target.khz()));
///             (next != chain.frequency).then_some(next)
///         });
///         Plan {
///             rail: None,
///             frequencies,
///             delay: Duration::from_secs(1),
///         }
///     }
/// }
///
/// let mut tuner = Raise {
///     increment: Frequency::from_mhz(25),
/// };
/// let mut chain = ChainReadings {
///     chips: 4,
///     frequency: Frequency::from_mhz(450),
///     hottest: None,
///     hashrate: 0,
/// };
/// let mut readings = Readings {
///     now: Instant::from_millis(0),
///     voltage: Voltage::from_mv(1200),
///     psu: None,
///     dangerous_chip_temp: None,
///     chains: [Some(chain), None],
/// };
/// let target = Frequency::from_mhz(460);
///
/// // The absent connector gets no command.
/// let plan = tuner.step(target, &readings, &());
/// assert_eq!(plan.frequencies, [Some(target), None]);
/// assert_eq!(plan.rail, None);
///
/// // At the target the plan has no command.
/// chain.frequency = target;
/// readings.chains[0] = Some(chain);
/// let plan = tuner.step(target, &readings, &());
/// assert_eq!(plan.frequencies, [None, None]);
/// ```
pub trait Tuner<const CHAINS: usize> {
    /// The operating point the tuner steers toward.
    type Target: Copy + PartialEq;
    /// Readings the platform reports beyond [`Readings`].
    type PlatformReadings;

    fn step(
        &mut self,
        target: Self::Target,
        readings: &Readings<CHAINS>,
        platform: &Self::PlatformReadings,
    ) -> Plan<CHAINS>;
}
