//! BMM100 BM1362 ASIC mining backend.
//!
//! Owns the chip-side serial protocol and the work-dispatch loop. Keeps the
//! tokio-era `mmm` log strings unchanged where possible so the `no_std` and
//! std implementations diff cleanly across both traces.

#![no_std]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::missing_panics_doc)]
#![allow(clippy::module_name_repetitions)]

pub mod bm1362;
pub mod consts;
pub mod serial;
