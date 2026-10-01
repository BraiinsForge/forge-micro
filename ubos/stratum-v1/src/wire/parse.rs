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

//! Stratum V1 RPC parser, no-std and borrow-only.

use serde::Deserialize;

use super::messages::{
    ClientReconnect, ConfigureResult, Notify, SetDifficulty, SetExtranonce, SetVersionMask,
    SubscribeResult,
};
use super::rpc::{Notification, Response, Rpc, StratumError};
use crate::error::ParseError;

#[cfg(test)]
#[path = "tests/parse.rs"]
mod tests;

// Extracts just the fields needed for the first pass.
#[derive(Deserialize)]
struct Probe<'a> {
    id: Option<u32>,
    method: Option<&'a str>,
}

// Response shape — params field is absent; result/error are captured by
// parse_response via find_field.
#[derive(Deserialize)]
struct RawResponse {
    id: u32,
}

// Wire format: [code, message] or [code, message, detail].
#[derive(Deserialize)]
struct ErrorArray<'a>(i32, &'a str, Option<&'a str>);

/// Parse one newline-delimited JSON-RPC line into an [`Rpc`].
///
/// A first pass reads `id` and `method` only, which is what tells a
/// response from a notification: a response carries `id` and no
/// `method`, and a notification carries `method`. The parser chosen from
/// that pair then reads the line again for the fields it needs.
///
/// # Errors
///
/// Returns [`ParseError::Json`] when the line is not JSON, and
/// [`ParseError::InvalidShape`] when it carries neither an `id` nor a
/// `method`.
pub fn parse_incoming(line: &[u8]) -> Result<Rpc<'_>, ParseError<'_>> {
    let (probe, _): (Probe<'_>, _) = serde_json_core::from_slice(line).map_err(ParseError::Json)?;

    match (probe.id, probe.method) {
        (Some(_), None) => parse_response(line),
        (_, Some(_)) => parse_notification(line),
        _ => Err(ParseError::InvalidShape),
    }
}

/// Parse a response line into [`Rpc::Response`].
///
/// `result` and `error` are captured as raw byte slices rather than
/// deserialised, because either can hold any JSON shape and the caller
/// is what decides how to read it.
///
/// # Errors
///
/// Returns [`ParseError::Json`] when the line is not JSON, and
/// [`ParseError::InvalidShape`] when both fields are absent or null.
fn parse_response(line: &[u8]) -> Result<Rpc<'_>, ParseError<'_>> {
    let (raw, _): (RawResponse, _) = serde_json_core::from_slice(line).map_err(ParseError::Json)?;

    let result_bytes = find_field(line, b"result");
    let error_bytes = find_field(line, b"error");

    // Normalise per spec §6.1: (Some, Some) keeps error and drops result.
    let (result, error) = match (result_bytes, error_bytes) {
        (_, Some(error)) if !is_null(error) => (None, Some(parse_stratum_error(error)?)),
        (Some(result), _) if !is_null(result) => (Some(result), None),
        _ => return Err(ParseError::InvalidShape),
    };

    Ok(Rpc::Response(Response {
        id: raw.id,
        stratum_result: result,
        stratum_error: error,
    }))
}

/// Parse the `error` field of a response into a [`StratumError`].
///
/// The standard shape is `[code, message]` or `[code, message, detail]`.
/// Some pools send a bare JSON string instead, which arrives with no
/// code, so the code reported for it is the generic one.
///
/// # Errors
///
/// Returns [`ParseError::InvalidShape`] on an empty slice, and
/// [`ParseError::Json`] when the bytes are neither a string nor the
/// expected array.
fn parse_stratum_error(raw: &[u8]) -> Result<StratumError<'_>, ParseError<'_>> {
    if raw.is_empty() {
        return Err(ParseError::InvalidShape);
    }

    // Some pools send a plain string here instead of the standard error
    // array, so check for one before deserialising.
    if raw[0] == b'"' {
        let (msg, _): (&str, _) = serde_json_core::from_slice(raw).map_err(ParseError::Json)?;
        // Use 20 ("Other/Unknown") as a generic error code since none was provided
        return Ok(StratumError(20, msg, None));
    }

    let (error, _): (ErrorArray<'_>, _) =
        serde_json_core::from_slice(raw).map_err(ParseError::Json)?;

    Ok(StratumError(error.0, error.1, error.2))
}

/// Parse a notification line into [`Rpc::Notification`].
///
/// The `params` value is located in the raw line and handed to the arm
/// for the named method, because each method carries a different tuple
/// and one deserialise cannot cover them all.
///
/// # Errors
///
/// Returns [`ParseError::MissingField`] when `method` or `params` is
/// absent, [`ParseError::Json`] when `params` does not match the shape
/// the method declares, and [`ParseError::UnknownMethod`] for a method
/// this client does not handle. A client-side method name reaches that
/// last arm too, since nothing pushes one at a client.
fn parse_notification(line: &[u8]) -> Result<Rpc<'_>, ParseError<'_>> {
    let (probe, _): (Probe<'_>, _) = serde_json_core::from_slice(line).map_err(ParseError::Json)?;

    let method_str = probe.method.ok_or(ParseError::MissingField("method"))?;

    let params = find_field(line, b"params").ok_or(ParseError::MissingField("params"))?;

    match method_str {
        "mining.notify" => {
            let (notify, _): (Notify<'_>, _) =
                serde_json_core::from_slice(params).map_err(ParseError::Json)?;
            Ok(Rpc::Notification(Notification::Notify(notify)))
        }
        "mining.set_difficulty" => {
            let (arr, _): ([f64; 1], _) =
                serde_json_core::from_slice(params).map_err(ParseError::Json)?;
            Ok(Rpc::Notification(Notification::SetDifficulty(
                SetDifficulty(arr[0]),
            )))
        }
        "mining.set_version_mask" => {
            let (arr, _): ([&str; 1], _) =
                serde_json_core::from_slice(params).map_err(ParseError::Json)?;
            let mask =
                u32::from_str_radix(arr[0], 16).map_err(|_| ParseError::MissingField("mask"))?;
            Ok(Rpc::Notification(Notification::SetVersionMask(
                SetVersionMask { mask },
            )))
        }
        "mining.set_extranonce" => {
            let (tuple, _): ((&str, u8), _) =
                serde_json_core::from_slice(params).map_err(ParseError::Json)?;
            Ok(Rpc::Notification(Notification::SetExtranonce(
                SetExtranonce {
                    extra_nonce_1: tuple.0,
                    extra_nonce_2_size: tuple.1,
                },
            )))
        }
        "mining.ping" => {
            let id = probe.id.ok_or(ParseError::MissingField("id"))?;
            Ok(Rpc::Notification(Notification::Ping { id }))
        }
        "client.reconnect" => {
            let (tuple, _): ((&str, u16, u16), _) =
                serde_json_core::from_slice(params).map_err(ParseError::Json)?;
            Ok(Rpc::Notification(Notification::ClientReconnect(
                ClientReconnect {
                    host: tuple.0,
                    port: tuple.1,
                    wait: tuple.2,
                },
            )))
        }
        other => Err(ParseError::UnknownMethod(other)),
    }
}

/// Parse a `mining.subscribe` result into its three elements.
///
/// The wire shape is `[subscriptions, extra_nonce_1,
/// extra_nonce_2_size]`. Element 0 is returned as raw bytes, because a
/// subscription entry is itself an array of arrays and no consumer here
/// reads it. All three are located by index rather than deserialised as
/// a tuple, since holding one element back is not something
/// `serde-json-core` can do.
///
/// # Errors
///
/// Returns [`ParseError::MissingField`] when any of the three elements
/// is absent, and [`ParseError::Json`] when element 1 is not a string
/// or element 2 is not a `u8`.
pub fn parse_subscribe_result(body: &[u8]) -> Result<SubscribeResult<'_>, ParseError<'_>> {
    // serde-json-core cannot skip or raw-borrow the first element of the
    // `[subscriptions, extra_nonce_1, extra_nonce_2_size]` wire tuple, so
    // each element is located by hand.
    let inner = body;
    let subs = find_array_element(inner, 0).ok_or(ParseError::MissingField("subscriptions"))?;

    let extra_nonce_1_raw =
        find_array_element(inner, 1).ok_or(ParseError::MissingField("extra_nonce_1"))?;
    let extra_nonce_2_size_raw =
        find_array_element(inner, 2).ok_or(ParseError::MissingField("extra_nonce2_size"))?;

    let (extra_nonce_1, _): (&str, _) =
        serde_json_core::from_slice(extra_nonce_1_raw).map_err(ParseError::Json)?;
    let (extra_nonce_2_size, _): (u8, _) =
        serde_json_core::from_slice(extra_nonce_2_size_raw).map_err(ParseError::Json)?;

    Ok(SubscribeResult(subs, extra_nonce_1, extra_nonce_2_size))
}

/// Wrap a `mining.configure` result body for later reading.
///
/// The body stays raw, because the negotiated extensions are a flat
/// object whose keys depend on what the client asked for.
/// [`parse_version_rolling_mask`] reads the one key this client needs.
///
/// # Errors
///
/// None are produced today. The `Result` is in the signature so that a
/// later extension can reject a body without moving every call site.
pub fn parse_configure_result(body: &[u8]) -> Result<ConfigureResult<'_>, ParseError<'_>> {
    Ok(ConfigureResult(body))
}

/// Extract the negotiated version-rolling mask from a `mining.configure`
/// result body. Returns `None` when `version-rolling` was rejected or the
/// pool uses an unsupported flat-object shape; Braiins and other public
/// pools serialise the mask as a hex string under the flat key
/// `"version-rolling.mask"`.
#[must_use]
pub fn parse_version_rolling_mask(body: &[u8]) -> Option<u32> {
    let value = find_field(body, b"version-rolling.mask")?;
    let hex = strip_json_string(value)?;
    parse_u32_hex(hex)
}

/// The bytes inside a JSON string token, without its quotes.
///
/// Returns `None` when `value` is not a quoted string. Escape sequences
/// are left as they are: the one caller reads hex digits, and no pool
/// escapes those.
fn strip_json_string(value: &[u8]) -> Option<&[u8]> {
    if value.len() < 2 || value[0] != b'"' || value[value.len() - 1] != b'"' {
        return None;
    }
    Some(&value[1..value.len() - 1])
}

/// Parse up to eight hex digits as a big-endian `u32`.
///
/// Returns `None` on an empty slice, on more than eight digits, and on
/// any byte that is not a hex digit. Both letter cases are accepted,
/// because the pool chooses which one it sends.
fn parse_u32_hex(hex: &[u8]) -> Option<u32> {
    if hex.is_empty() || hex.len() > 8 {
        return None;
    }
    let mut value: u32 = 0;
    for byte in hex {
        let nibble = match byte {
            b'0'..=b'9' => byte - b'0',
            b'a'..=b'f' => byte - b'a' + 10,
            b'A'..=b'F' => byte - b'A' + 10,
            _ => return None,
        };
        value = (value << 4) | u32::from(nibble);
    }
    Some(value)
}

/// Parse a result body that is a bare JSON boolean.
///
/// `mining.authorize` and `mining.submit` both answer this way.
///
/// # Errors
///
/// Returns [`ParseError::Json`] when the body is not a boolean.
pub fn parse_boolean_result(body: &[u8]) -> Result<bool, ParseError<'_>> {
    let (value, _): (bool, _) = serde_json_core::from_slice(body).map_err(ParseError::Json)?;
    Ok(value)
}

/// Value of the named object field, as a subslice of `json`.
///
/// Finds `"<key>":` and returns the bytes of the value token, which can
/// be an object, an array, a string or a primitive, up to the next comma
/// or brace at depth 0. Returns `None` when the key is absent.
///
/// The scan is textual and tracks no object depth, so it answers with
/// the first field of that name anywhere in the input, nested included.
/// Every caller passes a whole line from the pool and asks for a
/// top-level `result`, `error` or `params`, which no V1 message repeats
/// inside a value.
fn find_field<'a>(json: &'a [u8], key: &[u8]) -> Option<&'a [u8]> {
    let mut index = 0;
    while index < json.len() {
        if json[index] != b'"' {
            index += 1;
            continue;
        }
        index += 1;
        if index + key.len() > json.len() {
            break;
        }
        if &json[index..index + key.len()] != key {
            index = skip_string_from(json, index - 1)?;
            continue;
        }
        index += key.len();
        if index >= json.len() || json[index] != b'"' {
            continue;
        }
        index += 1;
        while index < json.len() && is_ws(json[index]) {
            index += 1;
        }
        if index >= json.len() || json[index] != b':' {
            continue;
        }
        index += 1;
        while index < json.len() && is_ws(json[index]) {
            index += 1;
        }
        let start = index;
        let end = skip_value(json, index)?;
        return Some(&json[start..end]);
    }
    None
}

/// Bytes of the element at `target_index` in a JSON array.
///
/// `json` must open with `[`, after optional whitespace. Returns `None`
/// when the input is not an array, when the array holds fewer than
/// `target_index + 1` elements, and when an element is malformed.
fn find_array_element(json: &[u8], target_index: usize) -> Option<&[u8]> {
    let mut index = 0;
    while index < json.len() && is_ws(json[index]) {
        index += 1;
    }
    if index >= json.len() || json[index] != b'[' {
        return None;
    }
    index += 1;
    let mut element_index = 0;
    loop {
        while index < json.len() && is_ws(json[index]) {
            index += 1;
        }
        if index >= json.len() {
            return None;
        }
        if json[index] == b']' {
            return None;
        }
        let start = index;
        let end = skip_value(json, index)?;
        if element_index == target_index {
            return Some(&json[start..end]);
        }
        element_index += 1;
        index = end;
        while index < json.len() && is_ws(json[index]) {
            index += 1;
        }
        if index >= json.len() {
            return None;
        }
        if json[index] == b',' {
            index += 1;
            continue;
        }
        return None;
    }
}

/// Index one past the JSON value that starts at `start`.
///
/// Covers a string, an array, an object and a primitive. Nested brackets
/// are counted, and a bracket inside a string is not. A primitive ends
/// at the first delimiter or whitespace byte. Returns `None` when the
/// value is unterminated.
fn skip_value(json: &[u8], start: usize) -> Option<usize> {
    let mut index = start;
    if index >= json.len() {
        return None;
    }
    match json[index] {
        b'"' => skip_string_from(json, index),
        b'[' | b'{' => {
            let open = json[index];
            let close = if open == b'[' { b']' } else { b'}' };
            index += 1;
            let mut depth: usize = 1;
            while index < json.len() {
                match json[index] {
                    b'"' => {
                        index = skip_string_from(json, index)?;
                        continue;
                    }
                    byte if byte == open => depth += 1,
                    byte if byte == close => {
                        depth -= 1;
                        if depth == 0 {
                            return Some(index + 1);
                        }
                    }
                    _ => {}
                }
                index += 1;
            }
            None
        }
        _ => {
            // Primitive (number, bool, null): advance until delimiter.
            while index < json.len() {
                match json[index] {
                    b',' | b'}' | b']' | b' ' | b'\t' | b'\r' | b'\n' => return Some(index),
                    _ => index += 1,
                }
            }
            Some(index)
        }
    }
}

/// Index one past the JSON string whose opening quote is at `start`.
///
/// A backslash escapes the byte after it, so an escaped quote does not
/// close the string. Returns `None` when the string is unterminated.
fn skip_string_from(json: &[u8], start: usize) -> Option<usize> {
    let mut index = start + 1;
    while index < json.len() {
        match json[index] {
            b'\\' => index += 2,
            b'"' => return Some(index + 1),
            _ => index += 1,
        }
    }
    None
}

/// Whether `byte` is whitespace that JSON allows between tokens.
fn is_ws(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\r' | b'\n')
}

/// Whether `value` is the JSON `null` literal.
///
/// A pool sends `"result": null` beside a real `error`, and the two
/// together are what [`parse_response`] has to tell apart.
fn is_null(value: &[u8]) -> bool {
    value == b"null"
}
