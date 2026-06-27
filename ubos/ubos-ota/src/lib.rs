#![no_std]
//! Deduplicated RP OTA flash-write target shared by the rp2350, rp2040, and
//! bmm-pico firmware crates. Implements `ubos_api::OtaTarget` on
//! `embassy-boot`; generic over flash size and the core-1 park poll delay.

mod shared_flash;
mod target;
pub use shared_flash::{SharedBlockingFlash, SharedFlashMutex};
pub use target::{OTA_BREADCRUMB_MAGIC, RpOtaTarget, mark_booted, record_panic_breadcrumb};

/// Parses a decimal `u32` from a build-time version string at compile time.
///
/// # Panics
///
/// Panics during the build on an empty string, a non-digit byte, or on
/// overflow, so a wrong version fails the compile rather than shipping
/// silently.
#[must_use]
pub const fn parse_version(raw: &str) -> u32 {
    let bytes = raw.as_bytes();
    assert!(!bytes.is_empty(), "UBOS_FW_VERSION must not be empty");
    let mut value: u32 = 0;
    let mut index = 0;
    while index < bytes.len() {
        let digit = bytes[index];
        assert!(digit.is_ascii_digit(), "UBOS_FW_VERSION must be decimal");
        value = value * 10 + (digit - b'0') as u32;
        index += 1;
    }
    value
}
