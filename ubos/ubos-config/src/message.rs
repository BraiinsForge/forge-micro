//! Wire-level command type for the config module.
//!
//! `Message` is the only thing senders construct. The API submits an
//! `Update(Config)` after validating the POST body; `run` performs
//! the reconfigure bracket against the manager.

use crate::{Config, NetworkConfig};

/// Wire-level command sent to the config module via `Handle::send`.
///
/// `Update` carries a full `Config` and dwarfs the `SetNetwork` and
/// `FactoryReset` variants; the enum is
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
    /// Replace only the network section and persist it. Network settings
    /// do not affect the hashing path, so this skips the reconfigure
    /// bracket: mining keeps running while the change is persisted and
    /// published. Used by the live network apply/confirm flow, where the
    /// IP was already switched at the stack and only the persisted record
    /// needs updating once the operator confirms reachability.
    SetNetwork(NetworkConfig),
    /// Wipe the persisted config back to [`Config::default`] and publish
    /// the defaults on [`STATE_WATCH`]. Does not trigger the reconfigure
    /// bracket because the API layer requests a reboot immediately after
    /// sending this message; the board never returns to normal operation
    /// with the fresh defaults.
    FactoryReset,
}
