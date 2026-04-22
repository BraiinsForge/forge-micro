//! Target-agnostic building blocks shared by every board crate.
//!
//! All boards run embassy-net with smoltcp for TCP/IP; the link layer
//! differs per board. The two `WIZnet` boards use `embassy-net-wiznet`
//! in MACRAW mode so the chip acts as a raw MAC and smoltcp owns the
//! stack. That gives one code path across W5500 and W5100S, full
//! DHCP/DNS/TCP/UDP, and removes the 8-socket ceiling of the `WIZnet`
//! hardware TCP/IP stack. The STM32 board uses its native RMII MAC
//! with a LAN8720 PHY through `embassy-stm32::eth`, also feeding
//! smoltcp.
//!
//! The HTTP response is hand-rolled into a caller-owned buffer rather
//! than routed through a framework like picoserve. A single metrics
//! page does not justify the code-size and compile-time overhead.
#![no_std]

pub mod btc;
pub mod consts;
pub mod display;
pub mod http;
pub mod led;
pub mod metrics;
pub mod net;
pub mod ntp;
pub mod sh1106;
pub mod uart_echo;
pub mod watchdog;

/// Seconds since Unix epoch if NTP has synced, or seconds since boot otherwise.
#[inline]
pub fn wall_clock_secs() -> u64 {
    embassy_time::Instant::now().as_secs()
        + net::NTP_EPOCH_OFFSET.load(portable_atomic::Ordering::Relaxed)
}
