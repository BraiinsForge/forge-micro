//! Pause reason types and bitmask type.
//!
//! `PauseReason` lists every condition that warrants a mining halt.
//! `PauseReasons` is the live bitmask: any non-empty mask means mining
//! is halted. Both are manager-private vocabulary: only `run_manager`
//! constructs them, from the `Message`s it receives.

/// Every condition that causes mining to halt.
///
/// Each variant maps to one bit in `PauseReasons`. Any non-empty
/// mask means mining must be halted; there is no observation-only
/// pause reason.
///
/// Discriminants are wire-format bit positions: `OtaUpdate` now occupies
/// the formerly-`Reconfiguring` bit 4 so the remaining variants keep
/// their existing positions. Do not renumber to close the gap.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum PauseReason {
    /// Chip temperature at or above the dangerous threshold.
    DangerousTemperature = 0,
    /// Fan tachometer signal lost or out-of-range.
    BrokenFan = 1,
    /// Operator-requested halt via `POST /api/system/pause`.
    UserRequest = 2,
    /// All configured pool entries are unreachable.
    PoolUnreachable = 3,
    /// An over-the-air firmware update is in progress and is erasing flash.
    OtaUpdate = 4,
    /// The tuner reported a fault it cannot recover from.
    TunerFault = 5,
}

/// Live bitmask of active pause reasons.
///
/// Any non-empty mask means mining is halted. Positions are hidden
/// behind the `assert`/`clear`/`contains`/`is_empty` API so callers
/// never manipulate bits directly.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct PauseReasons(u16);

impl PauseReasons {
    /// Set the bit for `reason`.
    #[must_use]
    pub fn assert(self, reason: PauseReason) -> Self {
        Self(self.0 | (1u16 << reason as u8))
    }

    /// Clear the bit for `reason`.
    #[must_use]
    pub fn clear(self, reason: PauseReason) -> Self {
        Self(self.0 & !(1u16 << reason as u8))
    }

    /// True when no reason is active (mining may run).
    #[must_use]
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// True when `reason` is currently asserted.
    #[must_use]
    pub fn contains(self, reason: PauseReason) -> bool {
        self.0 & (1u16 << reason as u8) != 0
    }
}
