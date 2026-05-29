//! Wire-level command type for the config module.
//!
//! `Message` is the only thing senders construct. The API submits an
//! `Update(Config)` after validating the POST body; `run` performs
//! the reconfigure bracket against the manager.

use crate::Config;

/// Wire-level command sent to the config module via `Handle::send`.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum Message {
    /// Replace the live config with a new value. Triggers the
    /// reconfigure bracket: `Reconfiguring(true)` to manager, wait
    /// for mining to halt, publish on `STATE_WATCH`, then
    /// `Reconfiguring(false)`.
    Update(Config),
}
