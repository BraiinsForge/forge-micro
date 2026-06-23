//! Network framework for the firmware: DHCP/smoltcp glue, periodic
//! NTP sync loop, stratum TCP connect helper, and the cross-board
//! atomic state (assigned IP, NTP offset, request count) read by
//! the dashboard and the Prometheus exposition layer.
//!
//! The mDNS responder and compile-time service registry have moved to
//! `ubos-mdns`. All boards run embassy-net with smoltcp for TCP/IP;
//! the link layer differs per board. The two `WIZnet` boards use
//! `embassy-net-wiznet` in MACRAW mode so the chip acts as a raw MAC
//! and smoltcp owns the stack. The STM32 boards use their native
//! RMII MAC through `embassy-stm32::eth`.
#![no_std]

pub mod config;
pub mod mac;
pub mod net;
pub mod ntp;

ubos_events::events! {
    ip_assigned(octet0: u8, octet1: u8, octet2: u8, octet3: u8) info = "IP assigned {}.{}.{}.{}",
}

/// Seconds since Unix epoch if NTP has synced, or seconds since boot otherwise.
#[inline]
#[must_use]
pub fn wall_clock_secs() -> u64 {
    embassy_time::Instant::now().as_secs() + net::handle().ntp_epoch_offset()
}
