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

//! Protocol-agnostic mining client trait and borrowed domain types.
//!
//! Defines the contract between protocol adapters (`stratum-v1`,
//! `stratum-v2`) and the integration layer (`stratum-client`).
//!
//! Architecture: `../../../docs/STRATUM_ARCHITECTURE.md`.
//! Design rationale: `../../../docs/plan/STRATUM_IMPLEMENTATION_PLAN.md`.

#![no_std]

mod client;
pub mod difficulty;
mod error;
mod extension;
pub mod hashing;
pub mod hex;
mod job;
mod job_id;
mod job_id_ring;
mod lazy_merkle;
mod session;
mod share;

pub use crate::client::{DriveEvent, MiningClient, MiningClientVisitor};
pub use crate::error::{Error, SessionRejection};
pub use crate::extension::ExtensionFrame;
pub use crate::job::MiningJob;
pub use crate::job_id::JobId;
pub use crate::job_id_ring::JobIdRing;
pub use crate::lazy_merkle::LazyMerkle;
pub use crate::session::{BIP320_VERSION_MASK, SessionUpdate, SubmitResult};
pub use crate::share::{MAX_EXTRANONCE_2, Share};
