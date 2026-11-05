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

//! Bodies of `GET /api/system/events` and `GET /api/system/events/catalog`.
//!
//! The handlers stream one record at a time, so each entry type here is
//! one array element and the array framing stays with the handler.
//! [`EventsResponse`] names the envelope for the document, and a test
//! holds the handler's framing bytes to it.

use serde::{Serialize, Serializer};
use ubos_events::{ArgWidth, EventDef, EventRecord, Severity};

/// Bytes in a record's argument payload.
const ARGS_LEN: usize = EventRecord::EMPTY.args.len();

/// The `{"events":[...]}` body of `GET /api/system/events`.
#[derive(Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct EventsResponse<'r> {
    /// Records after the cursor, oldest first.
    pub events: &'r [EventRecordJson<'r>],
}

/// One ring record. `args` is the raw payload as lower-case hex.
#[derive(Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct EventRecordJson<'r> {
    /// Position in the ring, increasing by one per record.
    pub seq: u32,
    /// Timestamp of the record in the firmware's event clock.
    pub ts: u32,
    /// Severity of the event.
    pub sev: ApiSeverity,
    /// FNV-1a hash of the event slug, resolved through the catalog.
    pub code: u32,
    /// Argument payload as lower-case hex, decoded with the widths the
    /// catalog lists.
    #[serde(serialize_with = "hex_bytes")]
    #[cfg_attr(feature = "schema", schemars(with = "str"))]
    pub args: &'r [u8; ARGS_LEN],
}

impl<'r> From<&'r EventRecord> for EventRecordJson<'r> {
    fn from(record: &'r EventRecord) -> Self {
        Self {
            seq: record.seq,
            ts: record.ts,
            sev: ApiSeverity::from(record.severity),
            code: record.code,
            args: &record.args,
        }
    }
}

/// One catalog entry: how to decode a record with this `code`.
#[derive(Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct EventDefJson {
    /// FNV-1a hash of the event slug.
    pub code: u32,
    /// Severity of the event.
    pub sev: ApiSeverity,
    /// Format string with one placeholder per arg.
    pub fmt: &'static str,
    /// Width of each arg in the payload, in order.
    #[serde(serialize_with = "arg_widths")]
    #[cfg_attr(feature = "schema", schemars(with = "&[ApiArgWidth]"))]
    pub args: &'static [ArgWidth],
}

impl From<&EventDef> for EventDefJson {
    fn from(def: &EventDef) -> Self {
        Self {
            code: def.code,
            sev: ApiSeverity::from(def.severity),
            fmt: def.fmt,
            args: def.args,
        }
    }
}

/// Severity of an event as the `sev` string.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum ApiSeverity {
    /// A state transition that needs no action.
    Info,
    /// A condition that warrants attention but did not stop mining.
    Warn,
    /// A condition that stopped mining or caused a fault.
    Error,
}

impl From<Severity> for ApiSeverity {
    fn from(severity: Severity) -> Self {
        match severity {
            Severity::Info => Self::Info,
            Severity::Warn => Self::Warn,
            Severity::Error => Self::Error,
        }
    }
}

/// A record stores its severity as the `Severity` repr byte: 1 is warn,
/// 2 is error, and anything else is info.
impl From<u8> for ApiSeverity {
    fn from(value: u8) -> Self {
        match value {
            1 => Self::Warn,
            2 => Self::Error,
            _ => Self::Info,
        }
    }
}

/// Width of one argument in a record's payload, as the catalog names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum ApiArgWidth {
    /// Unsigned 8-bit integer.
    U8,
    /// Unsigned 16-bit integer.
    U16,
    /// Unsigned 32-bit integer.
    U32,
    /// Signed 8-bit integer.
    I8,
    /// Signed 16-bit integer.
    I16,
    /// Signed 32-bit integer.
    I32,
    /// A temperature as a signed 32-bit integer in millidegrees Celsius.
    Temperature,
    /// A frequency as an unsigned 32-bit integer in kHz.
    Frequency,
    /// A voltage as an unsigned 32-bit integer in mV.
    Voltage,
    /// A power as an unsigned 32-bit integer in mW.
    Power,
    /// An efficiency as an unsigned 32-bit integer in mJ/TH.
    Efficiency,
}

impl From<ArgWidth> for ApiArgWidth {
    fn from(width: ArgWidth) -> Self {
        match width {
            ArgWidth::U8 => Self::U8,
            ArgWidth::U16 => Self::U16,
            ArgWidth::U32 => Self::U32,
            ArgWidth::I8 => Self::I8,
            ArgWidth::I16 => Self::I16,
            ArgWidth::I32 => Self::I32,
            ArgWidth::Temperature => Self::Temperature,
            ArgWidth::Frequency => Self::Frequency,
            ArgWidth::Voltage => Self::Voltage,
            ArgWidth::Power => Self::Power,
            ArgWidth::Efficiency => Self::Efficiency,
        }
    }
}

/// Serialize a byte payload as one lower-case hex string.
fn hex_bytes<S: Serializer>(bytes: &[u8; ARGS_LEN], serializer: S) -> Result<S::Ok, S::Error> {
    let mut text: heapless::String<{ 2 * ARGS_LEN }> = heapless::String::new();
    for byte in bytes {
        // Cannot fail: the buffer holds two characters per payload byte.
        let _ = core::fmt::Write::write_fmt(&mut text, format_args!("{byte:02x}"));
    }
    serializer.serialize_str(&text)
}

/// Serialize arg widths as an array of their names.
fn arg_widths<S: Serializer>(args: &[ArgWidth], serializer: S) -> Result<S::Ok, S::Error> {
    serializer.collect_seq(args.iter().map(|width| ApiArgWidth::from(*width)))
}
