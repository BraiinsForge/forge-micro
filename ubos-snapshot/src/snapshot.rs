// Copyright (C) 2026  Braiins Systems s.r.o.
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.
//
// Braiins Systems s.r.o. reserves the right to grant any party a license
// to this program, or any part thereof, under any terms, and such a grant
// shall be considered distinct from the grant above.

//! Deserialisation types for the machine-state snapshot JSON envelope.
//!
//! Only the sections this tool interprets directly get typed structs:
//! `meta`, the event log, the event catalog, the parsed metric entries,
//! and the collection-errors map. Every other section is kept as an
//! untyped [`serde_json::Value`] so a firmware field addition never
//! breaks parsing; [`crate::report`] flattens those sections
//! generically.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;

use serde::Deserialize;
use serde_json::Value;

/// Snapshot envelope version this tool understands.
pub const SUPPORTED_VERSION: u32 = 1;

/// Root envelope of a machine-state snapshot file.
#[derive(Deserialize)]
pub struct Snapshot {
    /// Envelope format version. Only [`SUPPORTED_VERSION`] is accepted.
    pub version: u32,
    /// Capture metadata: when and from where the snapshot was taken.
    pub meta: Meta,
    /// `GET /api/info` response, or absent on fetch failure.
    #[serde(default)]
    pub info: Option<Value>,
    /// `GET /api/config` response, or absent on fetch failure.
    #[serde(default)]
    pub config: Option<Value>,
    /// System information section, or absent on fetch failure.
    #[serde(default)]
    pub system_info: Option<Value>,
    /// ASIC detail section, or absent on fetch failure.
    #[serde(default)]
    pub asic: Option<Value>,
    /// Thermal detail section, or absent on fetch failure.
    #[serde(default)]
    pub thermal: Option<Value>,
    /// Cooling detail section, or absent on fetch failure.
    #[serde(default)]
    pub cooling: Option<Value>,
    /// Mining statistics section, or absent on fetch failure.
    #[serde(default)]
    pub statistics: Option<Value>,
    /// Pool scoreboard section, or absent on fetch failure.
    #[serde(default)]
    pub scoreboard: Option<Value>,
    /// Network configuration section, or absent on fetch failure.
    #[serde(default)]
    pub network: Option<Value>,
    /// Decoded event-log ring contents, or absent on fetch failure.
    #[serde(default)]
    pub events: Option<EventsSection>,
    /// Event catalog used to decode `events`, or absent on fetch failure.
    #[serde(default)]
    pub events_catalog: Option<Vec<CatalogEntry>>,
    /// Metric entries parsed by the browser from the Prometheus
    /// exposition text, or absent on fetch failure.
    #[serde(default)]
    pub metrics: Option<Vec<MetricEntry>>,
    /// Section name to error message, for sections that failed to fetch.
    #[serde(default)]
    pub errors: BTreeMap<String, String>,
}

impl Snapshot {
    /// Parses a snapshot from its JSON text and checks the envelope
    /// version.
    ///
    /// # Errors
    ///
    /// Returns an error if `text` is not valid JSON matching the
    /// envelope shape, or if its `version` is not [`SUPPORTED_VERSION`].
    pub fn parse(text: &str) -> Result<Snapshot, Box<dyn Error>> {
        let snapshot: Snapshot = serde_json::from_str(text)?;
        if snapshot.version != SUPPORTED_VERSION {
            return Err(Box::new(UnsupportedVersionError {
                found: snapshot.version,
            }));
        }
        Ok(snapshot)
    }
}

/// Capture metadata attached to every snapshot.
#[derive(Deserialize, Default)]
pub struct Meta {
    /// ISO-8601 timestamp of when the snapshot was captured.
    #[serde(default)]
    pub generated_at: Option<String>,
    /// Hostname or IP address of the captured device.
    #[serde(default)]
    pub hostname: Option<String>,
    /// URL of the dashboard page the snapshot was taken from.
    #[serde(default)]
    pub page_url: Option<String>,
    /// Browser user agent that captured the snapshot.
    #[serde(default)]
    pub user_agent: Option<String>,
}

/// Wrapper around the decoded event-log ring contents.
#[derive(Deserialize, Default)]
pub struct EventsSection {
    /// Individual event records, in ring order.
    #[serde(default)]
    pub events: Vec<EventRecordJson>,
}

/// One event record as served by `/api/system/events`.
#[derive(Deserialize)]
pub struct EventRecordJson {
    /// Monotonic ring sequence number.
    pub seq: u64,
    /// `wall_clock_secs()` at the moment the event was recorded.
    pub ts: i64,
    /// Severity string: `"info"`, `"warn"`, or `"error"`.
    pub sev: String,
    /// Stable event code, looked up against the catalog.
    pub code: u32,
    /// 22-char hex string packing the event's argument payload.
    pub args: String,
}

/// One event definition as served by `/api/system/events/catalog`.
#[derive(Deserialize)]
pub struct CatalogEntry {
    /// Stable event code this entry describes.
    pub code: u32,
    /// Declared severity string: `"info"`, `"warn"`, or `"error"`.
    ///
    /// Not currently rendered: the report tints event rows by the
    /// severity recorded on the event itself, which is what actually
    /// fired, rather than the catalog's current declaration.
    #[allow(
        dead_code,
        reason = "kept for JSON round-trip fidelity with the catalog wire format"
    )]
    pub sev: String,
    /// Positional `{}` format string.
    pub fmt: String,
    /// Argument width strings, in declaration order.
    #[serde(default)]
    pub args: Vec<String>,
}

/// One metric entry as parsed by the browser from a Prometheus
/// exposition line.
///
/// A successfully parsed line carries `name` (and, when present,
/// `labels`, `value`, and `timestamp_ms`); a line that did not match
/// the exposition shape carries only `raw`, the line verbatim.
#[derive(Deserialize, Default)]
pub struct MetricEntry {
    /// Metric name, the part before an optional `{...}` label set.
    #[serde(default)]
    pub name: Option<String>,
    /// Raw label text without the surrounding braces, present only if
    /// the line had labels.
    #[serde(default)]
    pub labels: Option<String>,
    /// Metric value, kept as its original text so `NaN`/`Inf` round-trip
    /// without needing float parsing.
    #[serde(default)]
    pub value: Option<String>,
    /// Millisecond unix timestamp trailing the value, if present.
    #[serde(default)]
    pub timestamp_ms: Option<u64>,
    /// Verbatim line text, present only if the line did not match the
    /// exposition shape.
    #[serde(default)]
    pub raw: Option<String>,
}

/// Snapshot envelope version is not [`SUPPORTED_VERSION`].
#[derive(Debug)]
pub struct UnsupportedVersionError {
    /// Version found in the envelope.
    pub found: u32,
}

impl fmt::Display for UnsupportedVersionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unsupported snapshot version {}, this tool understands version {SUPPORTED_VERSION}",
            self.found
        )
    }
}

impl Error for UnsupportedVersionError {}
