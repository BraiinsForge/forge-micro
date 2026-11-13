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

#![no_std]

//! Top-of-graph firmware assembly layer.
//!
//! Holds wiring shared by every board binary that must reference the
//! functional subsystem crates. Dependency edges in the workspace flow
//! downward, so anything naming all of `ubos-config`, `ubos-thermal`,
//! `ubos-mining`, `ubos-psu`, `ubos-manager`, `ubos-api`, `ubos-net`, and
//! `stratum-client` cannot live in the foundational crates without a cycle;
//! it lives here.
//!
//! The crate is integration-level by nature: it depends on hardware-coupled
//! crates such as `ubos-mining`, so it does not host-build and carries no
//! unit tests. Anything worth testing stays in the subsystem crates below.

pub mod events;
pub mod handles;
pub mod net;
pub mod serve;
pub mod stratum;
pub mod tasks;

pub use handles::{Handles, Runners};
pub use stratum::{MAX_POOLS, StratumShared, init_stratum, run_stratum_session};
/// Re-exported so a board names the protocol its sessions carry, or
/// `NoExtension`, without a direct `stratum-client` dependency.
pub use stratum_client::{NoExtension, SessionExtension};
/// The session a board allocates, named `AsicSession<POOL_SLOTS>`
/// without a direct `stratum-client` dependency. The alias fills in the
/// work receiver budget, one per chain up to the machine ceiling, so the
/// protocol crate never reads the hardware descriptor.
pub type AsicSession<const N: usize> =
    stratum_client::AsicSession<N, { ubos_metrics::chain::MAX_CHAINS }>;
/// Re-exported for the same reason: `serve_all` names the merged
/// listener's port, buffer sizes and timeouts through this path.
pub use ubos_http::consts as http_consts;
/// Re-exported because [`serve_all`](crate::serve_all) expands to uses of
/// all three. Reaching them through this crate is what lets a board serve
/// both listeners without naming `ubos-http` in its own manifest.
pub use ubos_http::{PrefixRouter, http_worker_task, serve_http_pool};
