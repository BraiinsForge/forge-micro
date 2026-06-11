//! Wire-level command type for the config module.
//!
//! `Message` is the only thing senders construct. The API submits an
//! `Update(Config)` after validating the POST body; `run` persists
//! and publishes the new value.

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
    /// Replace the live config with a new value. Persists the new
    /// value, publishes it on `STATE_WATCH`, and signals stratum to
    /// reconnect.
    Update(Config),
    /// Replace only the network section and persist it. Unlike `Update`,
    /// which signals stratum to reconnect, a network change only persists
    /// and publishes the new record without restarting stratum. Used by
    /// the live network apply/confirm flow, where the IP was already
    /// switched at the stack and only the persisted record needs updating
    /// once the operator confirms reachability.
    SetNetwork(NetworkConfig),
    /// Wipe the persisted config back to [`Config::default`] and publish
    /// the defaults on `STATE_WATCH`. The API layer requests a reboot
    /// immediately after sending this message; the board never returns to
    /// normal operation with the fresh defaults.
    FactoryReset,
}
