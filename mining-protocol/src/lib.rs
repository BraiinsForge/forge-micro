//! Protocol-agnostic mining client trait and borrowed domain types.
//!
//! Defines the contract between protocol adapters (`stratum-v1`,
//! `stratum-v2`) and the integration layer (`stratum-client`). See
//! `boards/eth-examples/IMPLEMENTATION_PLAN.md` and
//! `boards/eth-examples/ARCHITECTURE.md` for the design.

#![no_std]

mod client;
mod error;
pub mod hex;
mod job;
mod job_id;
mod lazy_merkle;
mod session;
mod share;

pub use crate::client::{MiningClient, MiningClientVisitor};
pub use crate::error::Error;
pub use crate::job::MiningJob;
pub use crate::job_id::JobId;
pub use crate::lazy_merkle::LazyMerkle;
pub use crate::session::{SessionUpdate, SubmitResult};
pub use crate::share::Share;
