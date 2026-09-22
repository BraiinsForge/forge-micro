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

//! Tuning policies driven by the miner through [`ubos_tuner::Tuner`].
//!
//! Policies calculate plans. The miner owns initialization, reading collection,
//! command ordering, cancellation, and teardown. Serial actors and the PSU own
//! their confirmed settings.
//!
//! [`MachineTuner`] is the seam between the two: [`crate::Miner::run`]
//! names the tuner type it builds, and runs whichever tuner that type
//! selects for the configuration.

use ubos_asic::HashboardDescriptor;
use ubos_config::{FrequencyVoltageTarget, Tuning};
use ubos_core::units::{Frequency, Voltage};
use ubos_tuner::{Plan, Readings};

/// Tuner-facing snapshot of the active frequency and voltage target.
///
/// `Copy` so the miner can pass it by value into the trait methods
/// without lifetime-managing a borrow against its own `Config`
/// subscription.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct FixedTarget {
    pub frequency: Frequency,
    pub voltage: Voltage,
}

impl From<FrequencyVoltageTarget> for FixedTarget {
    fn from(target: FrequencyVoltageTarget) -> Self {
        Self {
            frequency: Frequency::from_mhz(target.frequency_mhz),
            voltage: Voltage::from_mv(target.voltage_mv),
        }
    }
}

/// The tuners one machine can run.
///
/// [`Tuning`] names a kind of tuning and carries its target. Which tuner
/// serves a kind is the machine's own answer, and a machine need not
/// answer every kind, so [`select`](Self::select) may decline one.
///
/// Configuration belongs to the runner, outside the shared
/// [`Tuner`](ubos_tuner::Tuner) contract: an implementation converts the
/// configured target into its tuner's own target type, and reads
/// whatever its platform reports beyond [`Readings`]. The loop names
/// neither.
pub trait MachineTuner<const CHAINS: usize>: Sized {
    /// The tuner `tuning` selects on a machine whose hashboards are
    /// `board`, or `None` when the machine has no tuner for it. Called
    /// once per bring-up cycle.
    ///
    /// An implementation answers `Some` only for a variant its
    /// [`step`](Self::step) serves. The miner cannot check that: a
    /// machine that accepts a variant it then plans nothing for mines at
    /// whatever the bring-up left, indefinitely and without a word.
    ///
    /// `board` is the only descriptor `select` receives, so a policy that
    /// needs the chip's frequency grid or the rail ceiling, as the fixed
    /// policy does, is built from it. One that names its own machine can
    /// ignore it.
    fn select(tuning: &Tuning, board: &'static HashboardDescriptor) -> Option<Self>;

    /// Plan the next move.
    ///
    /// `tuning` is read again before every step, so a new target reaches
    /// the tuner here rather than through a restart. The miner passes
    /// only the variant [`select`](Self::select) accepted, because it
    /// builds a new tuner when the variant changes. An implementation
    /// still answers for any variant, since this trait is public and the
    /// miner is not its only possible caller; commanding nothing is the
    /// answer for a variant it has no target in.
    fn step(&mut self, tuning: &Tuning, readings: &Readings<CHAINS>) -> Plan<CHAINS>;
}

pub mod fixed;
