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

/// NTP time synchronization parameters.
pub struct Ntp;
impl Ntp {
    pub const HOST: &str = "pool.ntp.org";
    pub const PORT: u16 = 123;
    pub const SYNC_INTERVAL: Duration = Duration::from_secs(60);
    /// SNTP packet size: 48 bytes for both request and response.
    pub const PACKET_SIZE: usize = 48;
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
    /// Receive side of the scrape socket. 512 bytes covers the ~390-byte
    /// Prometheus 3.x request. Trimmed from 1024 so the enlarged
    /// `REQUEST_BUFFER_SIZE` below nets a RAM reduction overall: the rp2040
    /// runs the full miner firmware plus this server in 256 KiB and had
    /// only ~240 bytes of free SRAM, so every metrics static is sized
    /// against that ceiling.
    pub const RX_BUFFER_SIZE: usize = 512;
    /// Send side. Kept at 1024 so the 2 KiB exposition response leaves in
    /// two segments rather than four; only the receive side was trimmed.
    pub const TX_BUFFER_SIZE: usize = 1024;
    pub const RESPONSE_BUFFER_SIZE: usize = 2048;
    /// Buffer for parsing one scraper request. `edge-http` needs the whole
    /// request line plus header block to fit here at once, not just the
    /// `GET /metrics` line. A Prometheus 3.x scrape negotiates `OpenMetrics`
    /// and UTF-8 escaping in a ~240-byte Accept header, ~390 bytes of
    /// request in total; the former 256-byte buffer truncated that, header
    /// parsing failed, and the connection was dropped with no response,
    /// which the scraper saw as an EOF. 512 holds that request with
    /// headroom while staying within the rp2040 SRAM ceiling noted on
    /// `RX_BUFFER_SIZE`; 1024 overflowed that board by ~530 bytes.
    pub const REQUEST_BUFFER_SIZE: usize = 512;
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
