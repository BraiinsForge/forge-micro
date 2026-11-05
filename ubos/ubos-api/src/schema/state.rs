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

//! Enumerations the `/api/*` bodies report as strings.
//!
//! Each enum here is the wire form of a domain enum another crate owns,
//! converted by an exhaustive `From`, so the document lists its values
//! and the domain crates carry no serde dependency.

use serde::Serialize;
use ubos_manager::PauseReason;
use ubos_metrics::chain::ChainPresence;
use ubos_mining::MiningState;

/// A chain's presence as the `/api/info` `chains[].state` string.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum ApiChainState {
    /// No enumeration attempt has completed yet.
    Unknown,
    /// The chain answered with the expected chip count.
    Present,
    /// The connector answered with zero chips.
    Absent,
    /// The chain failed enumeration or its safe-boot configuration.
    Faulted,
}

/// The miner's lifecycle state as the `mining_state` string of both
/// info bodies.
// The metrics label spells the same strings through
// `MiningState::as_api_str`, and a test holds the two equal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum ApiMiningState {
    /// Chips are held in reset until a pool slot holds a job.
    WaitingForPool,
    /// Bring-up is in progress.
    Starting,
    /// Every present chain receives work.
    Running,
    /// Chips are held in reset, or no mining hardware is configured.
    Stopped,
}

impl From<MiningState> for ApiMiningState {
    fn from(state: MiningState) -> Self {
        match state {
            MiningState::WaitingForPool => Self::WaitingForPool,
            MiningState::Starting => Self::Starting,
            MiningState::Running => Self::Running,
            MiningState::Stopped => Self::Stopped,
        }
    }
}

/// Why mining is paused, as the `pause_reason` string of both info
/// bodies. The dashboard reads these strings as an enum, so a rename
/// is a schema change.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum ApiPauseReason {
    /// Chip temperature at or above the dangerous threshold.
    DangerousTemperature,
    /// Fan tachometer signal lost or out of range.
    BrokenFan,
    /// An operator asked for the halt.
    UserRequest,
    /// A firmware update is erasing flash.
    OtaUpdate,
    /// The tuner reported a fault it cannot recover from.
    TunerFault,
    /// A temperature sensor that owes a reading is not giving one.
    TemperatureUnreadable,
}

impl From<PauseReason> for ApiPauseReason {
    fn from(reason: PauseReason) -> Self {
        match reason {
            PauseReason::BrokenFan => Self::BrokenFan,
            PauseReason::UserRequest => Self::UserRequest,
            PauseReason::OtaUpdate => Self::OtaUpdate,
            PauseReason::TunerFault => Self::TunerFault,
        }
    }
}

impl From<ChainPresence> for ApiChainState {
    fn from(presence: ChainPresence) -> Self {
        match presence {
            ChainPresence::Unknown => Self::Unknown,
            ChainPresence::Present { .. } => Self::Present,
            ChainPresence::Absent => Self::Absent,
            ChainPresence::Faulted { .. } => Self::Faulted,
        }
    }
}
