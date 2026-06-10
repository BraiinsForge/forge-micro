//! Wire-level command type for the config module.
//!
//! `Message` is the only thing senders construct. The API submits an
//! `Update(Config)` after validating the POST body; `run` performs
//! the reconfigure bracket against the manager.

use crate::Config;

/// Wire-level command sent to the config module via `Handle::send`.
///
/// `Update` carries a full `Config` and dwarfs `FactoryReset`; the enum is
/// already `Config`-sized regardless, and this crate is `no_std` without
/// `alloc`, so boxing a variant is not an option.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum Message {
    /// Replace the live config with a new value. Triggers the
    /// reconfigure bracket: `Reconfiguring(true)` to manager, wait
    /// for mining to halt, publish on `STATE_WATCH`, then
    /// `Reconfiguring(false)`.
    Update(Config),
    /// Wipe the persisted config back to [`Config::default`] and publish
    /// the defaults on [`STATE_WATCH`]. Does not trigger the reconfigure
    /// bracket because the API layer requests a reboot immediately after
    /// sending this message; the board never returns to normal operation
    /// with the fresh defaults.
    FactoryReset,
}
