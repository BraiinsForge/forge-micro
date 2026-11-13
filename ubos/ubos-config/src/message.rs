// Copyright (C) 2026  Braiins Systems s.r.o.
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.
//
// Braiins Systems s.r.o. reserves the right to grant any party a license
// to this program, or any part thereof, under any terms, and such a grant
// shall be considered distinct from the grant above.

//! Wire-level command type for the config module.
//!
//! `Message` is the only thing senders construct. The API submits an
//! `Update(Config)` after validating the POST body; `run` persists
//! and publishes the new value.

use crate::{Config, NetworkConfig, StagedConfig};

/// Wire-level command sent to the config module via `Handle::send`.
///
/// `Update` carries a full `Config` and dwarfs the other variants;
/// the enum is already `Config`-sized regardless, and this crate is
/// `no_std` without `alloc`, so boxing a variant is not an option.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum Message {
    /// Replace the live config with a new value. Persists the new
    /// value, publishes it on the state watch, and signals stratum to
    /// reconnect.
    Update(Config),
    /// Apply a network change. With `confirm`, publish the change as a
    /// preview and arm the confirm-or-revert window without persisting;
    /// otherwise persist immediately. The live stack is switched by
    /// `ubos-net` watching the published config, so this message manages
    /// intent only and never touches the stack.
    ApplyNetwork {
        /// New network section to apply.
        network: NetworkConfig,
        /// When `true`, arm the confirm-or-revert window and persist only
        /// on confirm. When `false`, persist immediately with no window.
        confirm: bool,
    },
    /// Confirm a pending network preview, promoting it to the persisted
    /// config. A no-op when nothing is pending, so a DHCP lease arriving
    /// with no preview armed is harmless.
    ConfirmNetwork,
    /// Revert a pending network preview at once, republishing the
    /// committed config. A no-op when nothing is pending.
    RevertNetwork,
    /// Wipe the persisted config back to [`Config::default`] and publish
    /// the defaults on the state watch. The API layer requests a reboot
    /// immediately after sending this message; the board never returns to
    /// normal operation with the fresh defaults.
    FactoryReset,
    /// Store the record that the image an OTA staged reads, then answer
    /// [`Handle::downgrade`](crate::Handle::downgrade), which sends it. The
    /// OTA handler reboots into the image next, so the task stores nothing
    /// after it: a later write would replace the record the image reads.
    Downgrade(StagedConfig),
}
