#![no_std]
//! OTA flash-write targets for the ubos board firmware.
//!
//! The board-agnostic [`OtaEngine`] implements [`ubos_api::OtaTarget`] by
//! driving an [`OtaBackend`]; each board supplies a backend. The RP backend
//! ([`RpBackend`], feature `rp`) drives `embassy-boot`'s swap-based updater;
//! the ESP backend (feature `esp`) drives the esp-idf OTA partitions. The
//! engine owns the shared orchestration — write-offset tracking, breadcrumb
//! phase ordering, and the core-1 park lifecycle — so no board can diverge
//! from it.

mod engine;
pub use engine::{OtaBackend, OtaEngine};

#[cfg(feature = "rp")]
mod rp;
#[cfg(feature = "rp")]
mod shared_flash;
#[cfg(feature = "rp")]
pub use rp::{OTA_BREADCRUMB_MAGIC, RpBackend, mark_booted, record_panic_breadcrumb};
#[cfg(feature = "rp")]
pub use shared_flash::{SharedBlockingFlash, SharedFlashMutex};

/// The RP OTA target: the board-agnostic [`OtaEngine`] over the RP
/// `embassy-boot` [`RpBackend`]. Generic over flash size and the core-1 park
/// poll delay, matching the backend.
#[cfg(feature = "rp")]
pub type RpOtaTarget<const FLASH_SIZE: usize, const PARK_POLL_DELAY_CYCLES: u32> =
    OtaEngine<RpBackend<FLASH_SIZE, PARK_POLL_DELAY_CYCLES>>;

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
