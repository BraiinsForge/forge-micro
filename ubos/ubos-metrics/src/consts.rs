//! Tuning for the Prometheus metrics exposition listener on port 8081.

use embassy_time::Duration;

use ubos_core::consts::Net;

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
    /// Metrics exposition body buffer. 4096 holds the richer metric sets
    /// (dual voltage sources, build info) that overflow 2048. The smallest
    /// board, the rp2040, still has ~75 KiB of SRAM free at this size, so
    /// every board uses it; no per-board feature gate.
    pub const RESPONSE_BUFFER_SIZE: usize = 4096;
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
    /// Per-socket inactivity timeout for the scrape listener. The base
    /// value; a scrape completes in one request and the listener closes, so
    /// keepalive reclaim does not apply.
    pub const SOCKET_TIMEOUT: Duration = Net::SOCKET_TIMEOUT;
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
