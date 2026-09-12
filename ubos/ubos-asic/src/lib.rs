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

//! Pure chip-protocol library for the supported ASIC families.
//!
//! Holds the vendor-neutral chain seam ([`chain::ChainDriver`] over
//! [`transport::Transport`]) plus the [`bm13xx`] chip family (the
//! Bitmain register layouts, framing, and bring-up recipes). Additional
//! vendor drivers live in their own crates against this same seam. This
//! crate performs no I/O and holds no global state: drivers frame onto
//! the byte-level `Transport` the consumer implements, and the serial
//! actor in `ubos-mining` owns the UART and all atomics.

#![no_std]

#[cfg(test)]
#[path = "tests/support.rs"]
mod test_support;

pub mod bm13xx;
pub mod board;
pub mod chain;
pub mod pvt;
pub mod transport;

pub use bm13xx::{chip::Chip, driver::Bm13xxDriver};
pub use board::*;
pub use chain::{
    BIP320_VERSION_MASK, ChainDriver, ChainSolution, ChipDescriptor, ChipError, MAX_INGEST_CHUNK,
    MAX_SOLUTIONS_PER_BATCH, PollBatch, PollProgress, ResultMode, ShareDifficulty, ShareTarget,
    SolutionBatch, WorkJob, block_header, rolled_version, solution_hash,
};
pub use pvt::{ChainPvt, TemperatureSummary, VoltageSummary};
pub use transport::{BaudRateError, Transport};
