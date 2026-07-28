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

//! Pure chip-protocol library for the `BM13xx` ASIC family.
//!
//! Holds the per-chip register layouts, serial framing, hashboard
//! descriptors, and the per-chip bring-up recipes. This crate performs
//! no I/O and holds no global state: bring-up drives the [`chip::ChipBus`]
//! trait the consumer implements, and the serial actor in `ubos-mining`
//! owns the UART and all atomics.

#![no_std]

pub(crate) mod baud;
pub mod board;
pub mod chain;
pub mod chip;
pub mod framing;
pub mod registers;
pub mod transport;

pub use board::*;
pub use chain::{
    BIP320_VERSION_MASK, ChainDriver, ChainSolution, MAX_SOLUTIONS_PER_BATCH,
    ResultMode, SolutionBatch, WorkJob, block_header, rolled_version, solution_hash,
};
pub use chip::{
    BM1362, BM1366, BM1370, Bm1362, Bm1366, Bm1370, Chip, ChipBus, ChipDescriptor, ChipError,
    ChipProtocol, MAX_CHAIN_CHIPS,
};
pub use framing::*;
pub use registers::*;
pub use transport::{BaudRateError, Transport};
