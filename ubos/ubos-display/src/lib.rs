//! OLED display driver and render task for the firmware framework.
//!
//! `sh1106` is a minimal SPI driver for SH1106-compatible 128x64
//! panels. `display` is the periodic render task that reads
//! cross-board atomics (IP, NTP offset, fan PWM/RPM, hashrate) and
//! draws them onto the panel.
#![no_std]

pub mod display;
pub mod sh1106;
