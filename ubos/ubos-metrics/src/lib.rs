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

//! Observability: the metric types, the metrics store and its
//! exposition.
//!
//! The crate is the store alone, so it stays a leaf that every crate
//! can push into. The module that owns a state calculates it and pushes
//! it: an estimator, a sampler, a table of another module's state and a
//! publisher task all live in the crate that owns the source. The miner
//! holds the hashrate estimator and the accounting of each chain, and
//! each platform crate holds its idle counter.
//!
//! The `metrics` feature keeps the store: the channel, the series map
//! and the render buffer. A build without it frees the RAM they hold,
//! and a push returns at once.
#![no_std]

pub mod catalog;
pub mod consts;
mod freshness;
pub mod metrics;
mod name;
#[cfg(feature = "metrics")]
mod render;
#[cfg(feature = "metrics")]
mod store;
pub mod value;

pub use metrics::Metric;
pub use name::{Name, Unit};
