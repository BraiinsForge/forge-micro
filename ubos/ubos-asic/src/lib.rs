//! Pure chip-protocol library for the `BM13xx` ASIC family.
//!
//! Holds the BM1362 register layouts, serial framing, and hashboard
//! descriptors. This crate performs no I/O and holds no global state;
//! the serial actor in `ubos-mining` owns the UART and all atomics.

#![no_std]

pub mod framing;
pub mod registers;

pub use framing::*;
pub use registers::*;
