//! Miner orchestration configuration.
//!
//! Defines [`TuningAlgorithm`] and [`MinerConfig`], which name the
//! bring-up strategy the mining task constructs at each chip bring-up
//! cycle.

/// Tuner algorithm the miner constructs at bring-up.
///
/// `ubos-mining` reads this to decide which `Tuner` implementation
/// to construct at bring-up. A live change tears down the chip and
/// restarts under the new tuner — switching algorithms requires a
/// full PSU + reset cycle, not an in-place swap.
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    postcard_schema::Schema,
)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum TuningAlgorithm {
    /// Open-loop frequency ramp built on the BM1362 PLL calculator
    /// and the `HashCountingNumber` formula. Reaches arbitrary targets
    /// and is the default tuner.
    #[default]
    Fixed,
}

/// Miner-level orchestration. Separate from `TuningConfig` because
/// it does not describe what the tuner aims for — it describes which
/// tuner instance the miner constructs.
#[derive(
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    postcard_schema::Schema,
)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct MinerConfig {
    /// Tuner algorithm selected for the next bring-up.
    pub algorithm: TuningAlgorithm,
}

impl MinerConfig {
    /// Validate the miner config.
    ///
    /// The algorithm field is an enum with no out-of-range values;
    /// this method exists for symmetry with the other per-domain
    /// validators so `Config::validate` and `ConfigUpdate::validate`
    /// can call it without special-casing.
    ///
    /// # Errors
    ///
    /// Always returns `Ok(())` for now.
    pub fn validate(&self) -> Result<(), &'static str> {
        Ok(())
    }
}
