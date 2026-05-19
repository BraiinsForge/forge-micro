//! Shared configuration constants grouped by subsystem namespace.

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
    /// Sized to hold the inlined dashboard HTML plus headers in a single
    /// response, comfortably under the TCP initial congestion window.
    pub const RESPONSE_BUFFER_SIZE: usize = 6144;
    /// Buffer for reading incoming HTTP requests, discarded after the
    /// request line is parsed for routing.
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

/// mDNS/DNS-SD responder settings per RFC 6762 and RFC 6763.
pub struct Mdns;
impl Mdns {
    /// A record TTL in seconds. RFC 6762 §10 recommends 120 s for volatile
    /// addresses such as DHCP leases.
    pub const TTL_SECS: u32 = 120;
    /// DNS-SD service type for the Prometheus metrics scrape target.
    pub const PROMETHEUS_SERVICE: &str = "_prometheus";
    /// DNS-SD service type for the board HTTP UI.
    pub const HTTP_SERVICE: &str = "_http";
    /// Transport protocol used by every service the responder advertises.
    pub const TCP_PROTOCOL: &str = "_tcp";
    /// URL path advertised in the Prometheus service TXT record.
    pub const METRICS_PATH: &str = "/metrics";
    /// URL path advertised in the HTTP service TXT record.
    pub const HTTP_PATH: &str = "/";
    /// Seconds to wait before retrying after a bind or run error.
    pub const RETRY_DELAY_SECS: u64 = 5;
    /// Buffer size for mDNS datagrams. RFC 6762 §17 caps mDNS messages at
    /// the interface UDP payload max.
    pub const BUFFER_SIZE: usize = 1472;
    /// Packet-metadata slots per direction for the mDNS UDP socket.
    pub const METADATA_SLOTS: usize = 4;
    /// Sockets in the UDP pool. One is sufficient because the responder
    /// holds a single socket for the lifetime of each run attempt.
    pub const SOCKET_POOL_SIZE: usize = 1;
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
