//! BMM101 BM1362 ASIC mining backend.
//!
//! Owns the chip-side serial protocol and the work-dispatch loop.
//! [`Handle`] is the public surface every other crate uses to
//! observe state and issue commands. [`Miner`] is the typed state
//! machine that firmware constructs and drives; [`MinerControl`]
//! is the trait the tuner consumes. [`tuner`] hosts the `Tuner`
//! trait and its implementations.

#![no_std]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::missing_panics_doc)]
#![allow(clippy::module_name_repetitions)]

pub mod bus;
pub mod consts;
mod control;
pub mod miner;
pub mod publishers;
pub mod reset;
pub mod scheduler;
pub mod serial;
pub mod tuner;

pub use control::{Handle, Message, MiningState, MiningSubscription, handle};
pub use miner::{Miner, MinerControl, MinerError, PsuErrorKind};
pub use serial::{core1_is_parked, release_core1_park, request_core1_park};

ubos_events::events! {
    miner_reset()                                 info  = "Miner reset",
    psu_enabled()                                 info  = "PSU enabled",
    tuner_initialised()                           info  = "Tuner initialised",
    tuner_fault()                                 error = "Tuner fault",
    chip_count_mismatch(found: u8, expected: u8)  error = "Chip count {} differs from expected {}",
    frequency_ramp_started(freq_khz: u32)         info  = "Frequency ramp started to {} kHz",
    frequency_ramp_complete(freq_khz: u32)        info  = "Frequency ramp complete at {} kHz",
}
