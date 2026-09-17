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

//! Two-phase chip-tuning contract driven by [`crate::Miner`].
//!
//! [`Tuner`] is the trait every tuner implementation satisfies.
//! [`TuningConfig`] is the tuner-facing snapshot of the configured
//! target; the miner converts the storage-side
//! `ubos_config::TuningConfig` from raw `u32` kHz / mV into this
//! type's `Frequency` / `Voltage` units before each call.
//! `ubos-mining` depends on `ubos-config` directly; the edge does
//! not cycle because `ubos-config` no longer depends on
//! `ubos-mining` or `ubos-manager`.
//!
//! Tuner implementations live in submodules: `fixed::FixedTuner` is
//! the typed open-loop ramp that steps toward arbitrary targets via
//! `ChainControl::set_frequency` on every chain `MinerControl::present_chains`
//! yields, behind which the vendor-neutral chain driver owns the PLL
//! programming. The active tuner is runtime-selectable via
//! `MinerConfig::algorithm`.
//!
//! Enumeration, chain presence, and the enumeration retry policy are
//! the miner's business, not the tuner's: a tuner only ever sees the
//! chains the miner already found present.

use ubos_core::units::{Frequency, Voltage};

use crate::miner::{MinerControl, MinerError};

/// Tuner-facing snapshot of the active frequency and voltage target.
///
/// `Copy` so the miner can pass it by value into the trait methods
/// without lifetime-managing a borrow against its own `Config`
/// subscription.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct TuningConfig {
    pub frequency: Frequency,
    pub voltage: Voltage,
}

/// Two-phase contract: `init` brings the chip from the safe-boot
/// state the miner leaves it in to the configured target; `deinit`
/// returns the chip to a state where the PSU can be dropped cleanly.
///
/// Each method takes `&impl MinerControl`. The tuner never sees the
/// serial actor, never subscribes to `Config`, never selects on
/// signals; the miner drives both methods imperatively. Live tuning
/// updates on a running chip are not part of the contract; tuning
/// changes apply on the next bring-up.
///
/// Both methods report [`MinerError`] and there is no tuner-specific
/// error type. `Miner` folds every serial-bus and PSU failure into that
/// one enum before a tuner can see one, and enumeration, which was the
/// only failure a tuner used to own, is the miner's now.
// The async-fn-in-trait pattern matches the project-wide convention
// established by `ubos_psu::Psu` and `crate::miner::MinerControl`.
// Send bounds are not required on this embedded target; the trait is
// only called from a single-task context.
#[allow(async_fn_in_trait)]
pub trait Tuner<M: MinerControl> {
    /// Bring the chip from the safe-boot baseline to `target`. Every
    /// chain `miner.present_chains()` yields is configured, on the fast
    /// UART, and has one temperature reading behind it when this runs.
    async fn init(&mut self, miner: &M, target: TuningConfig) -> Result<(), MinerError>;

    /// Return the chip to a state where the PSU can be dropped.
    async fn deinit(&mut self, miner: &M) -> Result<(), MinerError>;
}

pub mod fixed;
