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

//! Decoding for the packed event-log argument payload and message
//! templates.
//!
//! Ported from the browser decoder in
//! `ubos-webui/web/components/Events.tsx` (`decodeArgs`,
//! `formatMessage`, `formatTime`) so the CLI renders event rows
//! identically to the dashboard, without a JS runtime.

use std::error::Error;
use std::fmt;

/// Width of a single decoded event argument.
///
/// Mirrors `ubos_events::ArgWidth`, as serialised into the event catalog
/// by `ubos-api`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Width {
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
}

impl Width {
    /// Parses one of the six catalog width strings.
    ///
    /// # Errors
    ///
    /// Returns an error if `value` is not exactly one of `"u8"`, `"u16"`,
    /// `"u32"`, `"i8"`, `"i16"`, or `"i32"`.
    pub fn parse(value: &str) -> Result<Width, UnknownWidthError> {
        match value {
            "u8" => Ok(Width::U8),
            "u16" => Ok(Width::U16),
            "u32" => Ok(Width::U32),
            "i8" => Ok(Width::I8),
            "i16" => Ok(Width::I16),
            "i32" => Ok(Width::I32),
            other => Err(UnknownWidthError(other.to_string())),
        }
    }

    /// Number of bytes this width occupies in the packed payload.
    #[must_use]
    fn byte_count(self) -> usize {
        match self {
            Width::U8 | Width::I8 => 1,
            Width::U16 | Width::I16 => 2,
            Width::U32 | Width::I32 => 4,
        }
    }

    /// Whether this width sign-extends its decoded value.
    #[must_use]
    fn is_signed(self) -> bool {
        matches!(self, Width::I8 | Width::I16 | Width::I32)
    }
}

/// A catalog width string other than `"u8"`, `"u16"`, `"u32"`, `"i8"`,
/// `"i16"`, or `"i32"`.
#[derive(Debug)]
pub struct UnknownWidthError(String);

impl fmt::Display for UnknownWidthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown argument width {:?}", self.0)
    }
}

impl Error for UnknownWidthError {}

/// The packed argument payload could not be decoded against the widths
/// requested.
#[derive(Debug)]
pub struct DecodeArgsError(String);

impl fmt::Display for DecodeArgsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Error for DecodeArgsError {}

/// Decodes and formats event-log records.
///
/// A zero-sized type rather than free functions, so the ported decoder
/// reads as a small owned facility instead of loose module functions.
pub struct EventCodec;

impl EventCodec {
    /// Decodes a 22-char hex payload into `widths.len()` signed values.
    ///
    /// Each width is read little-endian starting where the previous one
    /// left off; signed widths sign-extend into the returned `i64`. This
    /// mirrors the browser's `decodeArgs`.
    ///
    /// # Errors
    ///
    /// Returns an error if `hex` has an odd length, contains a
    /// non-hex-digit, or is shorter than the byte count `widths` needs.
    pub fn decode_args(hex: &str, widths: &[Width]) -> Result<Vec<i64>, DecodeArgsError> {
        let bytes = Self::hex_to_bytes(hex)?;
        let mut offset = 0usize;
        let mut values = Vec::with_capacity(widths.len());
        for width in widths {
            let byte_count = width.byte_count();
            let end = offset.checked_add(byte_count).ok_or_else(|| {
                DecodeArgsError(format!("argument offset overflow decoding {hex:?}"))
            })?;
            let slice = bytes.get(offset..end).ok_or_else(|| {
                DecodeArgsError(format!(
                    "payload {hex:?} is too short for a {width:?} at byte offset {offset}"
                ))
            })?;
            let mut accumulated: u64 = 0;
            for (index, byte) in slice.iter().enumerate() {
                accumulated |= u64::from(*byte) << (index * 8);
            }
            values.push(Self::sign_extend(accumulated, *width));
            offset = end;
        }
        Ok(values)
    }

    /// Sign-extends an accumulated little-endian value per its width.
    ///
    /// `accumulated` never exceeds 32 bits (the widest width is `u32`),
    /// so both branches fit losslessly in an `i64`.
    #[must_use]
    #[allow(
        clippy::cast_possible_wrap,
        reason = "accumulated is at most 32 bits wide, well within i64 range"
    )]
    fn sign_extend(accumulated: u64, width: Width) -> i64 {
        if width.is_signed() {
            let bits = width.byte_count() * 8;
            let sign_bit = 1u64 << (bits - 1);
            if accumulated & sign_bit != 0 {
                (accumulated as i64) - (1i64 << bits)
            } else {
                accumulated as i64
            }
        } else {
            accumulated as i64
        }
    }

    /// Parses a hex string into bytes, two characters at a time.
    fn hex_to_bytes(hex: &str) -> Result<Vec<u8>, DecodeArgsError> {
        if !hex.len().is_multiple_of(2) {
            return Err(DecodeArgsError(format!(
                "hex payload {hex:?} has odd length"
            )));
        }
        let bytes = hex.as_bytes();
        let mut out = Vec::with_capacity(bytes.len() / 2);
        for pair in bytes.chunks(2) {
            let pair_str = std::str::from_utf8(pair)
                .map_err(|_| DecodeArgsError(format!("hex payload {hex:?} is not ASCII")))?;
            let byte = u8::from_str_radix(pair_str, 16).map_err(|_| {
                DecodeArgsError(format!("hex payload {hex:?} contains a non-hex digit"))
            })?;
            out.push(byte);
        }
        Ok(out)
    }

    /// Substitutes each `{}` placeholder in `fmt` with the corresponding
    /// value, in order. Placeholders past the end of `values` render as
    /// empty, matching the browser's `formatMessage`.
    #[must_use]
    pub fn format_message(fmt: &str, values: &[i64]) -> String {
        let mut out = String::with_capacity(fmt.len());
        let mut rest = fmt;
        let mut index = 0usize;
        while let Some(pos) = rest.find("{}") {
            out.push_str(&rest[..pos]);
            if let Some(value) = values.get(index) {
                out.push_str(&value.to_string());
            }
            index += 1;
            rest = &rest[pos + 2..];
        }
        out.push_str(rest);
        out
    }

    /// Formats an event timestamp. Values below one billion are seconds
    /// since boot recorded before the clock synced to NTP; at or above
    /// that threshold they are unix seconds. The pre-sync branch mirrors
    /// the browser's `formatTime`; the post-sync branch renders a fixed
    /// UTC calendar timestamp rather than the browser's viewer-local
    /// `toLocaleString`, since a CLI cannot know the viewer's timezone.
    #[must_use]
    pub fn format_time(ts: i64) -> String {
        if ts >= 1_000_000_000 {
            Self::format_utc(ts)
        } else {
            format!("{ts}s (pre-sync)")
        }
    }

    /// Formats a unix-seconds timestamp as `YYYY-MM-DD HH:MM:SS UTC`.
    #[must_use]
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "ts is a positive unix-seconds value well within i64 range"
    )]
    fn format_utc(ts: i64) -> String {
        let days = ts.div_euclid(86_400);
        let secs_of_day = ts.rem_euclid(86_400) as u64;
        let hour = secs_of_day / 3600;
        let minute = (secs_of_day % 3600) / 60;
        let second = secs_of_day % 60;
        let (year, month, day) = Self::civil_from_days(days);
        format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02} UTC")
    }

    /// Days-since-1970-01-01 to proleptic Gregorian (year, month, day).
    ///
    /// Howard Hinnant's date algorithm; valid for every value an `i64`
    /// day count can hold. Locals (`z`, `doe`, `yoe`, `doy`, `mp`) keep
    /// the names from the published reference so the implementation
    /// reads against the paper.
    #[must_use]
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_possible_wrap,
        clippy::cast_sign_loss,
        reason = "bounded by the day count of a unix-seconds timestamp"
    )]
    fn civil_from_days(z: i64) -> (i64, u32, u32) {
        let z = z + 719_468;
        let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
        let doe = (z - era * 146_097) as u64;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let mut year = yoe as i64 + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
        let month = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
        if month <= 2 {
            year += 1;
        }
        (year, month, day)
    }
}

#[cfg(test)]
#[path = "tests/events.rs"]
mod tests_events;
