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
//! [`MachineTuner`] is the seam between the two: the firmware names the
//! machine's tuner set as the type parameter of [`crate::Miner`], and the
//! miner runs whichever tuner that set selects for the configuration.

use ubos_asic::HashboardDescriptor;
use ubos_config::{FrequencyVoltageTarget, Tuning};
use ubos_core::units::{Efficiency, Frequency, Power, Voltage};
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
            frequency: Frequency::from_khz(target.frequency_khz),
            voltage: Voltage::from_mv(target.voltage_mv),
        }
    }
}

/// Tuner-facing snapshot of the active [`ubos_config::PowerTarget`].
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct PowerTarget {
    pub input_power: Power,
    pub efficiency: Efficiency,
}

impl From<ubos_config::PowerTarget> for PowerTarget {
    fn from(target: ubos_config::PowerTarget) -> Self {
        Self {
            input_power: Power::from_mw(target.input_power_mw),
            efficiency: Efficiency::from_mj_per_th(target.efficiency_mj_per_th),
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
/// configured target into its tuner's own target type. The miner reads
/// what the platform reports beyond [`Readings`] through a function the
/// firmware supplies, and passes it as `platform`.
pub trait MachineTuner<const CHAINS: usize>: Sized {
    /// What the machine's platform reports beyond [`Readings`].
    type PlatformReadings;

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
    fn step(
        &mut self,
        tuning: &Tuning,
        readings: &Readings<CHAINS>,
        platform: &Self::PlatformReadings,
    ) -> Plan<CHAINS>;
}

/// The fixed and the power policy, for a machine that serves both kinds.
///
/// Each policy is a tuner set of its own that declines the other kind,
/// for a machine that serves only its kind. This set delegates to them,
/// so each policy alone decides which kind it serves.
pub enum Tuners {
    Fixed(fixed::FixedTuner),
    Power(power::PowerTuner),
}

impl<const CHAINS: usize> MachineTuner<CHAINS> for Tuners {
    type PlatformReadings = ();

    fn select(tuning: &Tuning, board: &'static HashboardDescriptor) -> Option<Self> {
        match tuning {
            Tuning::Fixed(_) => MachineTuner::<CHAINS>::select(tuning, board).map(Self::Fixed),
            Tuning::Power(_) => MachineTuner::<CHAINS>::select(tuning, board).map(Self::Power),
        }
    }

    fn step(&mut self, tuning: &Tuning, readings: &Readings<CHAINS>, (): &()) -> Plan<CHAINS> {
        match self {
            Self::Fixed(tuner) => tuner.step(tuning, readings, &()),
            Self::Power(tuner) => tuner.step(tuning, readings, &()),
        }
    }
}

pub mod fixed;
pub mod power;
