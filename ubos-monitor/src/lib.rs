//! Board-thermal monitor: PID fan controller plus thermal-threshold guard.
//!
//! Sibling of `ubos-backend`. Reads the board temperature sensor through
//! the shared `TemperatureSensor` trait, drives a board-supplied
//! `FanOutput` via PID, and exposes `HALT_MINING` for a real ASIC
//! backend to consult.

#![no_std]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::missing_panics_doc)]
#![allow(clippy::module_name_repetitions)]
