//! BMM100 BM1362 ASIC mining backend.
//!
//! Owns the chip-side serial protocol and the work-dispatch loop.

#![no_std]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::missing_panics_doc)]
#![allow(clippy::module_name_repetitions)]

pub mod bm1362;
pub mod bmm100_trace;
pub mod consts;
pub mod lm75a;
pub mod miner;
pub mod replay;
pub mod reset;
pub mod serial;
