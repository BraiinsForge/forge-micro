//! BMM100 BM1362 ASIC mining backend.
//!
//! Owns the chip-side serial protocol and the work-dispatch loop.
//! [`Handle`] is the public surface every other crate uses to
//! observe state and issue commands.

#![no_std]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::missing_panics_doc)]
#![allow(clippy::module_name_repetitions)]

pub mod bm1362;
pub mod bmm100_trace;
pub mod consts;
mod control;
pub mod miner;
pub mod publishers;
pub mod replay;
pub mod reset;
pub mod serial;

pub use control::{Handle, Message, MiningState, MiningSubscription, handle};
