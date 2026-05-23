//! Subsystem-scoped constants, the hardware-watchdog feed loop, and
//! the status-LED blink loop. The "core" of the firmware framework:
//! every other ubos-* crate depends on this one for shared values
//! and the LED/watchdog task wrappers.
#![no_std]

#[cfg(feature = "ccmram-bss")]
pub mod ccmram;
pub mod consts;
pub mod fan_state;
pub mod led;
pub mod watchdog;
