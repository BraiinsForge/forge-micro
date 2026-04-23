//! Shared configuration values grouped by subsystem. Unit structs
//! (`Net`, `Http`, `Tls`, `Ntp`, `Btc`, `Watchdog`, `Metrics`) act as
//! namespaces for constants so call sites read `Ntp::PORT` instead of
//! a flat `NTP_PORT`. Values are referenced from [`crate::net`],
//! [`crate::metrics`], and [`crate::watchdog`].

use embassy_time::Duration;

/// TCP/UDP socket timeouts shared across NTP and HTTP.
pub struct Net;
impl Net {
    pub const SOCKET_TIMEOUT: Duration = Duration::from_secs(10);
}

/// Plain HTTP server settings.
pub struct Http;
impl Http {
    pub const PORT: u16 = 80;
    pub const RX_BUFFER_SIZE: usize = 4096;
    pub const TX_BUFFER_SIZE: usize = 4096;
    /// HTTP response assembly buffer, separate from the TCP socket buffers.
    pub const RESPONSE_BUFFER_SIZE: usize = 1024;
    /// Buffer for reading incoming HTTP requests, discarded after accept.
    pub const REQUEST_BUFFER_SIZE: usize = 1024;
}

/// TLS connection buffer sizes for the BTC price fetch.
pub struct Tls;
impl Tls {
    pub const READ_BUFFER_SIZE: usize = 16640;
    pub const WRITE_BUFFER_SIZE: usize = 1024;
    pub const TCP_BUFFER_SIZE: usize = 4096;
    /// Buffer for reading TLS HTTP response bodies such as the Binance price API.
    pub const RESPONSE_BUFFER_SIZE: usize = 1024;
}

/// NTP time synchronization parameters.
pub struct Ntp;
impl Ntp {
    pub const HOST: &str = "pool.ntp.org";
    pub const PORT: u16 = 123;
    pub const SYNC_INTERVAL: Duration = Duration::from_secs(60);
    /// SNTP packet size: 48 bytes for both request and response.
    pub const PACKET_SIZE: usize = 48;
}

/// Binance BTC/USDT price polling.
pub struct Btc;
impl Btc {
    pub const HOST: &str = "api.binance.com";
    pub const PORT: u16 = 443;
    pub const REQUEST: &[u8] = b"GET /api/v3/ticker/price?symbol=BTCUSDT HTTP/1.0\r\nHost: api.binance.com\r\nConnection: close\r\n\r\n";
    pub const FETCH_INTERVAL: Duration = Duration::from_secs(300);
}

/// Hardware watchdog feed timing.
pub struct Watchdog;
impl Watchdog {
    pub const FEED_PERIOD: Duration = Duration::from_millis(500);
}

/// Internal chip temperature sampling.
pub struct Temperature;
impl Temperature {
    pub const PERIOD: Duration = Duration::from_secs(1);
    /// Prometheus convention prefers base SI units, which would make
    /// `_celsius` the canonical suffix. This crate targets the RP2040
    /// (Cortex-M0+, no FPU) among others, so a float-valued celsius
    /// gauge would pull software floating-point into the exposition
    /// path. Integer millicelsius preserves sub-degree resolution
    /// without soft-float, and the name reflects the unit carried on
    /// the wire.
    pub const METRIC_NAME: &str = "board_chip_temperature_millicelsius";
}

/// Prometheus metrics exposition server settings.
pub struct Metrics;
impl Metrics {
    pub const PORT: u16 = 8081;
    pub const RX_BUFFER_SIZE: usize = 2048;
    pub const TX_BUFFER_SIZE: usize = 2048;
    pub const RESPONSE_BUFFER_SIZE: usize = 2048;
    pub const REQUEST_BUFFER_SIZE: usize = 256;
    pub const CHANNEL_DEPTH: usize = 32;
    pub const MAX_METRICS: usize = 16;
    pub const MAX_LABELS: usize = 4;
}
