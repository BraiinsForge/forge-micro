//! Shared configuration constants grouped by subsystem namespace.

use embassy_time::Duration;

use crate::units::{Frequency, Voltage};

/// TCP/UDP socket timeouts shared across NTP and HTTP.
pub struct Net;
impl Net {
    pub const SOCKET_TIMEOUT: Duration = Duration::from_secs(10);
}

/// Plain HTTP server settings for the web-UI listener on port 80.
pub struct Http;
impl Http {
    pub const PORT: u16 = 80;
    /// Per-direction TCP socket buffer for the HTTP listener. 2 KiB is
    /// ~1.4× Ethernet MSS (1460), enough to hold a full response segment
    /// plus headers without inflating .bss on the 128 KiB stm32f407.
    pub const RX_BUFFER_SIZE: usize = 2048;
    pub const TX_BUFFER_SIZE: usize = 2048;
    /// Buffer for reading incoming HTTP requests. Sized to hold a small
    /// POST body alongside headers so the config form fits in a single
    /// read; the route line is parsed from the start of the same buffer.
    pub const REQUEST_BUFFER_SIZE: usize = 2048;
}

/// REST API listener settings (port 8080).
///
/// Sibling of `Http` so the REST surface can be sized independently of
/// the web-UI shell. Buffer sizes mirror `Http` today; tune separately if
/// REST payloads diverge from the SPA's needs.
pub struct Api;
impl Api {
    pub const PORT: u16 = 8080;
    pub const RX_BUFFER_SIZE: usize = 2048;
    pub const TX_BUFFER_SIZE: usize = 2048;
    pub const REQUEST_BUFFER_SIZE: usize = 2048;
}

/// TLS connection buffer sizes for the BTC price fetch.
pub struct Tls;
impl Tls {
    pub const READ_BUFFER_SIZE: usize = 16640;
    pub const WRITE_BUFFER_SIZE: usize = 1024;
    pub const TCP_BUFFER_SIZE: usize = 2048;
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

/// BM1362 mining defaults shared by `ubos-config` and `ubos-mining`.
/// Hoisted here so the boot-default frequency and voltage have one
/// source of truth; `Config::default()` reads them through
/// `.mhz()`/`.mv()`, and `ubos-mining` re-exports the typed constants
/// directly.
pub struct Mining;
impl Mining {
    /// Default mining frequency programmed at chip init.
    pub const DEFAULT_FREQUENCY: Frequency = Frequency::from_mhz(525);
    /// Default ASIC core voltage programmed at chip init.
    pub const DEFAULT_VOLTAGE: Voltage = Voltage::from_mv(1400);
}

/// Prometheus metrics exposition server settings.
pub struct Metrics;
impl Metrics {
    pub const PORT: u16 = 8081;
    pub const RX_BUFFER_SIZE: usize = 1024;
    pub const TX_BUFFER_SIZE: usize = 1024;
    pub const RESPONSE_BUFFER_SIZE: usize = 2048;
    pub const REQUEST_BUFFER_SIZE: usize = 256;
    pub const CHANNEL_DEPTH: usize = 32;
    /// Maximum number of distinct metric series held in the exposition
    /// map. Must be a power of two (heapless `FnvIndexMap` invariant).
    /// The map is keyed by the fully label-expanded series, so per-CPU
    /// and per-LM75A label combinations each consume one slot. 64 is
    /// ~2.5x headroom over the 25 label-expanded series pushed today
    /// while staying well within the .bss budget on the smallest
    /// targets (stm32f407 at 128 KiB, rp2040 at 256 KiB). An earlier
    /// 128-entry trial blew the stm32f4 SRAM; see the Footprint section
    /// of `docs/STRUCTURE.md` for the per-board RAM snapshot.
    pub const MAX_METRICS: usize = 64;
    pub const MAX_LABELS: usize = 4;
}
