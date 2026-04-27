//! Pure thermal-threshold evaluator.
//!
//! Splits hot/dangerous/cooldown decisions out of the embassy task so
//! they can be unit-tested with plain values, with no embassy executor
//! or atomics in the loop.

/// Temperature thresholds. All values in degrees Celsius.
pub struct Limits {
    pub hot_temp_c: f32,
    pub dangerous_temp_c: f32,
    pub cooldown_temp_c: f32,
}

/// What the monitor task should do for the current sample.
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    /// Below `hot`, or above `cooldown` while halted: nothing to do.
    Ok,
    /// Above `hot` but not yet `dangerous`: log a warning.
    Hot,
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
pub fn evaluate(temp_c: f32, halted: bool, limits: &Limits) -> Action {
    if halted {
        if temp_c <= limits.cooldown_temp_c {
            return Action::Resume;
        }
        return Action::Ok;
    }
    if temp_c >= limits.dangerous_temp_c {
        return Action::EnterHalt;
    }
    if temp_c >= limits.hot_temp_c {
        return Action::Hot;
    }
    Action::Ok
}
