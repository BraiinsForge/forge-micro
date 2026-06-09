//! Response schema for `GET /api/system/network`.

use serde::Serialize;

/// Live network status returned by `GET /api/system/network`.
///
/// `mode` reflects the currently configured IP assignment mode.
/// `pending` is `true` while a live change is awaiting operator
/// confirmation. `seconds_remaining` counts down to the automatic
/// revert; it is 0 when no change is pending.
#[derive(Serialize)]
pub struct SystemNetwork<'a> {
    /// Current network mode: `"dhcp"` or `"static"`.
    pub mode: &'a str,
    /// Whether a live change is awaiting confirmation.
    pub pending: bool,
    /// Seconds remaining in the confirm window, or 0 when nothing is pending.
    pub seconds_remaining: u64,
}
