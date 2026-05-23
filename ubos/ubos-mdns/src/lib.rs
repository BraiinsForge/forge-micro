//! mDNS responder loop plus the `ServiceEntry` type that crates use to
//! advertise themselves.
//!
//! Each crate that owns a network-visible port declares an entry at
//! module scope via [`declare_service!`]. The macro expands to a plain
//! `const ServiceEntry`. Firmware composes a `&[&ServiceEntry]` slice
//! at the [`run_mdns`] call site, so the firmware decides which
//! services the board actually advertises — no linker contract, no
//! startup copy.
#![no_std]

pub mod registry;
pub mod responder;

pub use registry::ServiceEntry;
pub use responder::{compose_hostname, run_mdns};
