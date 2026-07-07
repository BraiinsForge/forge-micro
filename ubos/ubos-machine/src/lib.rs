//! Machine descriptors: the miner modeled as composed data.
//!
//! A [`MachineDescriptor`] ties together the component descriptors owned
//! by the subsystem crates — the [`HashboardDescriptor`] from
//! `ubos-asic`, the [`BmmPsuDescriptor`] from `ubos-psu`, and the
//! [`SensorDescriptor`] table from `ubos-temperature` — with a
//! [`ControlBoardDescriptor`] and [`PlatformDescriptor`] that describe
//! the board the firmware runs on.
//!
//! This crate is pure `const` data with no hardware or async
//! dependencies, so both the firmware binaries and the host runner build
//! from the same definitions. It sits above the component crates and
//! below the binaries and the host runner, which read the machine they
//! build from here. The firmware binary supplies the typed peripheral
//! bindings that this data cannot express; see `docs/MACHINE.md`.

#![no_std]

pub mod control_board;
pub mod machine;
pub mod platform;

pub use control_board::*;
pub use machine::*;
pub use platform::*;
