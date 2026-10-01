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

//! Commands to the config task.

use crate::{Config, NetworkConfig, StagedConfig};

/// A command to the config task. `Update` carries a whole `Config` and
/// sizes the enum; there is no `alloc` to box it.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum Message {
    Update(Config),
    /// Apply a network change; `ubos-net` switches the stack from the
    /// publish.
    ApplyNetwork {
        network: NetworkConfig,
        /// Arm the confirm-or-revert window instead of persisting at once.
        confirm: bool,
    },
    /// Persist the pending preview; a no-op when none is pending.
    ConfirmNetwork,
    /// Drop the pending preview; a no-op when none is pending.
    RevertNetwork,
    /// Wipe the persisted config back to [`Config::for_board`] and publish
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
