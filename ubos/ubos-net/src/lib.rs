//! Network framework for the firmware: DHCP/smoltcp glue, periodic
//! NTP and BTC fetch loops, mDNS responder, and the cross-board
//! atomic state (assigned IP, NTP offset, BTC price, request count)
//! read by the dashboard and the Prometheus exposition layer.
//!
//! All boards run embassy-net with smoltcp for TCP/IP; the link
//! layer differs per board. The two `WIZnet` boards use
//! `embassy-net-wiznet` in MACRAW mode so the chip acts as a raw MAC
//! and smoltcp owns the stack. The STM32 boards use their native
//! RMII MAC through `embassy-stm32::eth`.
#![no_std]

pub mod mdns;
pub mod net;
pub mod ntp;

/// Seconds since Unix epoch if NTP has synced, or seconds since boot otherwise.
#[inline]
pub fn wall_clock_secs() -> u64 {
    embassy_time::Instant::now().as_secs()
        + net::NTP_EPOCH_OFFSET.load(portable_atomic::Ordering::Relaxed)
}
