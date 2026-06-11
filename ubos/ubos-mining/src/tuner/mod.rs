//! Two-phase chip-tuning contract driven by [`crate::Miner`].
//!
//! [`Tuner`] is the trait every tuner implementation satisfies.
//! [`TuningConfig`] is the tuner-facing snapshot of the configured
//! target; the miner translates the storage-side
//! `ubos_config::TuningConfig` into this type before each call so
//! `ubos-mining` does not depend on `ubos-config`; the reverse edge
//! runs transitively through `ubos-manager`, so a direct
//! `ubos-mining → ubos-config` link would cycle.
//!
//! Tuner implementations live in submodules: `replay::ReplayTuner`
//! walks the captured BMM100 trace through `MinerControl`'s
//! byte-level escape hatches. Additional tuners can live alongside
//! it; the active tuner is runtime-selectable via
//! `MinerConfig::algorithm`.

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

/// Two-phase contract: `init` brings the chip from the post-reset
/// state to the configured target; `deinit` returns the chip to a
/// state where the PSU can be dropped cleanly.
///
/// Each method takes `&impl MinerControl`. The tuner never sees the
/// serial actor, never subscribes to `Config`, never selects on
/// signals; the miner drives both methods imperatively. Live tuning
/// updates on a running chip are not part of the contract; tuning
/// changes apply on the next bring-up.
// The async-fn-in-trait pattern matches the project-wide convention
// established by `ubos_psu::Psu` and `crate::miner::MinerControl`.
// Send bounds are not required on this embedded target; the trait is
// only called from a single-task context.
#[allow(async_fn_in_trait)]
pub trait Tuner<M: MinerControl> {
    /// Bring the chip from the post-reset baseline to `target`.
    async fn init(&mut self, miner: &M, target: TuningConfig) -> Result<(), TunerError>;

    /// Return the chip to a state where the PSU can be dropped.
    async fn deinit(&mut self, miner: &M) -> Result<(), TunerError>;
}

/// Failure modes a tuner method can surface to the miner.
///
/// `Miner` wraps every serial-bus error into [`MinerError::Serial`]
/// before it reaches the tuner, so `TunerError::Miner` covers every
/// chip-side failure with a single variant.
#[derive(Debug)]
pub enum TunerError {
    Miner(MinerError),
}

impl From<MinerError> for TunerError {
    fn from(error: MinerError) -> Self {
        Self::Miner(error)
    }
}

pub mod replay;
