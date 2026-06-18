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
//! the typed open-loop ramp that reaches arbitrary targets via the
//! BM1362 PLL calculator. The active tuner is runtime-selectable via
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
/// chip-side failure. `ChipCountMismatch` is terminal: the miner stops
/// rather than retrying when the chain does not match the board.
#[derive(Debug)]
pub enum TunerError {
    Miner(MinerError),
    /// The PLL calculator rejected the requested frequency as outside
    /// the chip's supported range.
    UnsupportedFrequency(Frequency),
    /// Enumeration kept finding a chip count other than the board's
    /// expected `num_chips` after every retry.
    ChipCountMismatch {
        found: u8,
        expected: u8,
    },
}

impl From<MinerError> for TunerError {
    fn from(error: MinerError) -> Self {
        Self::Miner(error)
    }
}

impl From<ubos_asic::ChipError> for TunerError {
    fn from(error: ubos_asic::ChipError) -> Self {
        match error {
            ubos_asic::ChipError::UnsupportedFrequency(freq) => Self::UnsupportedFrequency(freq),
        }
    }
}

pub mod fixed;
