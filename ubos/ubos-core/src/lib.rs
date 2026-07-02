//! Subsystem-scoped constants, the hardware-watchdog feed loop, the
//! status-LED blink loop, and the hold-to-trigger button monitor. The
//! "core" of the firmware framework: every other ubos-* crate depends on
//! this one for shared values and the LED/watchdog/button task wrappers.
#![no_std]

pub mod button;
#[cfg(feature = "ccmram-bss")]
pub mod ccmram;
pub mod consts;
pub mod led;
pub mod reboot;
pub mod units;
pub mod watchdog;
