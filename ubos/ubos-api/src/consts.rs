//! Tuning for the REST API listener on port 8080.

use embassy_time::Duration;

use ubos_core::consts::Net;

/// REST API listener settings (port 8080).
///
/// Buffer sizes are sized independently of the web-UI shell in
/// `ubos-webui`; they mirror it today but tune separately if REST
/// payloads diverge from the SPA's needs.
pub struct Api;
impl Api {
    pub const PORT: u16 = 8080;
    // Kept at or below the W6100's 4 KiB on-chip RX buffer on purpose. Each
    // 4 KiB DFU sector write during an OTA upload pauses both cores, and the
    // W6100 cannot be drained while frozen; a window larger than the chip
    // buffer lets more data arrive than the chip can hold, so frames are
    // dropped and TCP backs off. A 2 KiB window keeps in-flight data under the
    // chip buffer so a single flash freeze cannot overflow it.
    pub const RX_BUFFER_SIZE: usize = 2048;
    pub const TX_BUFFER_SIZE: usize = 2048;
    pub const REQUEST_BUFFER_SIZE: usize = 2048;
    /// Per-socket inactivity timeout for the REST listener. Held at the base
    /// `Net::SOCKET_TIMEOUT` because an OTA upload streams here and each
    /// flash-write freeze stalls the socket; the timeout resets on progress,
    /// so a slow link survives as long as bytes keep arriving. The listener
    /// closes after every response, so keepalive reclaim does not apply.
    pub const SOCKET_TIMEOUT: Duration = Net::SOCKET_TIMEOUT;
}
