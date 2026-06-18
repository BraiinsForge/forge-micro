//! Pure chip-protocol library for the `BM13xx` ASIC family.
//!
//! Holds the per-chip register layouts, serial framing, hashboard
//! descriptors, and the per-chip bring-up recipes. This crate performs
//! no I/O and holds no global state: bring-up drives the [`chip::ChipBus`]
//! trait the consumer implements, and the serial actor in `ubos-mining`
//! owns the UART and all atomics.

#![no_std]

pub mod chip;
pub mod framing;
pub mod registers;

pub use chip::{Bm1362, Bm1366, Bm1370, Chip, ChipBus, ChipError, ChipProtocol, MAX_CHAIN_CHIPS};
pub use framing::*;
pub use registers::*;
