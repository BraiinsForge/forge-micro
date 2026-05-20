//! UART-attached ASIC peer for the `stratum-client` stack.
//!
//! Mirrors [`asic_cpu_mock::run_asic_cpu`] in shape: it polls the
//! work cell of an [`AsicSession`], encodes each fresh
//! [`WorkAssignment`] into a UART frame for an external peer (the
//! `dummy-asic` firmware), waits for the peer's `RawShare` reply,
//! and pushes the result back into the session.
//!
//! The wire protocol is the same as the legacy
//! `ubos_main::uart_asic` module — one Work frame out, one
//! Share frame back, XOR-8 checksum, resync on stray bytes — so a
//! board running the new stack and a peer running the old
//! `dummy-asic` firmware stay interoperable.

#![no_std]

mod run;
mod split;
mod wire;

#[cfg(test)]
#[path = "tests/wire.rs"]
mod tests;

pub use run::run_asic_uart;
pub use split::SplitUart;
pub use wire::BAUD_RATE;
