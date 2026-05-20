//! Target-agnostic building blocks shared by every board crate.
//!
//! Architecture: `../docs/OVERVIEW.md`.
#![no_std]

pub mod cpu_load;
pub mod hashrate;
pub mod http;
pub mod mdns;
pub mod metrics;
pub mod net;
pub mod ntp;

/// Seconds since Unix epoch if NTP has synced, or seconds since boot otherwise.
#[inline]
pub fn wall_clock_secs() -> u64 {
    embassy_time::Instant::now().as_secs()
        + net::NTP_EPOCH_OFFSET.load(portable_atomic::Ordering::Relaxed)
}
