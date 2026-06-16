//! Pure thermal-threshold evaluator.
//!
//! Splits dangerous/cooldown decisions out of the embassy task so they
//! can be unit-tested with plain values, with no embassy executor or
//! atomics in the loop.

/// What the thermal task should do for the current sample.
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    /// Below `dangerous`, or above `cooldown` while halted: nothing to do.
    Ok,
    /// Crossed `dangerous`: halt mining via `MiningHandle::halt` and
    /// log an error. Only returned on the rising edge into the halt
    /// state.
    EnterHalt,
    /// Below `cooldown` while halted: resume mining via
    /// `MiningHandle::resume` and log a recovery message.
    Resume,
}

/// Decide what action to take given the current sample and whether the
/// halt flag is currently set.
///
/// `dangerous_temp_c` is the halt threshold straight from the config; the
/// config layer bounds it (`ThermalConfig::validate` caps it), so there is
/// no separate ceiling here.
#[must_use]
pub fn evaluate(temp_c: f32, halted: bool, dangerous_temp_c: f32, cooldown_temp_c: f32) -> Action {
    if halted {
        if temp_c <= cooldown_temp_c {
            return Action::Resume;
        }
        return Action::Ok;
    }
    if temp_c >= dangerous_temp_c {
        return Action::EnterHalt;
    }
    Action::Ok
}
