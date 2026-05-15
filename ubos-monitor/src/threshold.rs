//! Pure thermal-threshold evaluator.
//!
//! Splits dangerous/cooldown decisions out of the embassy task so they
//! can be unit-tested with plain values, with no embassy executor or
//! atomics in the loop.

/// Board temperature configuration. Bundles the PID setpoint with the
/// safety thresholds so a single struct names every temperature the
/// monitor cares about. Mirrors the BOS `TempControlConfig` shape.
/// `evaluate` only reads `dangerous_temp_c` and `cooldown_temp_c`;
/// `target_temp_c` is consumed by the PID setpoint at construction.
pub struct TempConfig {
    pub target_temp_c: f32,
    pub dangerous_temp_c: f32,
    pub cooldown_temp_c: f32,
}

/// What the monitor task should do for the current sample.
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    /// Below `dangerous`, or above `cooldown` while halted: nothing to do.
    Ok,
    /// Crossed `dangerous`: set `HALT_MINING` and log an error. Only
    /// returned on the rising edge into the halt state.
    EnterHalt,
    /// Below `cooldown` while halted: clear `HALT_MINING` and log a
    /// recovery message.
    Resume,
}

/// Decide what action to take given the current sample and whether the
/// halt flag is currently set.
#[must_use]
pub fn evaluate(temp_c: f32, halted: bool, config: &TempConfig) -> Action {
    if halted {
        if temp_c <= config.cooldown_temp_c {
            return Action::Resume;
        }
        return Action::Ok;
    }
    if temp_c >= config.dangerous_temp_c {
        return Action::EnterHalt;
    }
    Action::Ok
}
