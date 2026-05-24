//! Response shape for `GET /api/system/statistics`.
//!
//! The spec calls for a `{currentTimestamp, labels[], statistics[][]}`
//! matrix. The structure is published with a fixed label set and an
//! empty rows slice. A server-side ring buffer can be wired later
//! without a schema change.

use serde::Serialize;

/// Column labels for the statistics matrix.
///
/// The order here is the column order clients must use when
/// interpreting rows in `SystemStatistics::statistics`.
pub const LABELS: &[&str] = &["hashrate", "temperature", "fan_rpm", "fan_pwm", "power_mw"];

/// Statistics time-series snapshot for `GET /api/system/statistics`.
///
/// `statistics` is a slice of row slices; each row contains one
/// sample per label in [`LABELS`] order. Until the server-side ring
/// buffer lands, `statistics` is always empty.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemStatistics<'response> {
    /// Unix epoch timestamp in milliseconds at the time of the
    /// response; zero if NTP has not yet synchronized.
    pub current_timestamp: u64,
    /// Column label names; stable across firmware updates.
    pub labels: &'static [&'static str],
    /// Sample rows, one entry per historical snapshot. Empty until the
    /// server-side ring buffer is wired in a later phase.
    pub statistics: &'response [&'response [i64]],
}
