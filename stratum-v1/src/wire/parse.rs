//! Stratum V1 RPC parser, no-std and borrow-only.

use serde::Deserialize;

use crate::error::ParseError;
use super::messages::{
    ClientReconnect, ConfigureResult, Notify, SetDifficulty, SetExtranonce, SetVersionMask,
    SubscribeResult,
};
use super::rpc::{Notification, Response, Rpc, StratumError};

#[cfg(test)]
#[path = "tests/parse.rs"]
mod tests;

// Used to extract just the fields we need in the first pass.
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

pub fn parse_incoming(line: &[u8]) -> Result<Rpc<'_>, ParseError<'_>> {
    let (probe, _): (Probe<'_>, _) = serde_json_core::from_slice(line).map_err(ParseError::Json)?;

    match (probe.id, probe.method) {
        (Some(_), None) => parse_response(line),
        (_, Some(_)) => parse_notification(line),
        _ => Err(ParseError::InvalidShape),
    }
}

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

fn parse_stratum_error(raw: &[u8]) -> Result<StratumError<'_>, ParseError<'_>> {
    if raw.is_empty() {
        return Err(ParseError::InvalidShape);
    }

    // Check if the pool sent a plain string instead of the standard error array
    // (ckpool I'm looking at you)
    if raw[0] == b'"' {
        let (msg, _): (&str, _) = serde_json_core::from_slice(raw).map_err(ParseError::Json)?;
        // Use 20 ("Other/Unknown") as a generic error code since none was provided
        return Ok(StratumError(20, msg, None));
    }

    let (error, _): (ErrorArray<'_>, _) =
        serde_json_core::from_slice(raw).map_err(ParseError::Json)?;

    Ok(StratumError(error.0, error.1, error.2))
}

fn parse_notification(line: &[u8]) -> Result<Rpc<'_>, ParseError<'_>> {
    let (probe, _): (Probe<'_>, _) = serde_json_core::from_slice(line).map_err(ParseError::Json)?;

    let method_str = probe.method.ok_or(ParseError::MissingField("method"))?;

    // Extract the "params" field value from the raw line.
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
        // Anything unknown at the server push level is an UnknownMethod error.
        // Client-side method names (subscribe, authorize, etc.) fall here too.
        other => Err(ParseError::UnknownMethod(other)),
    }
}

pub fn parse_subscribe_result(body: &[u8]) -> Result<SubscribeResult<'_>, ParseError<'_>> {
    // Wire: [subscriptions_array, extra_nonce_1, extra_nonce_2_size]
    // We parse as a 3-tuple where element 0 is the subscriptions raw slice.
    // serde-json-core cannot skip/raw the first element, so we find it manually.
    let inner = body; // body is the full result value, e.g. [[...], "abc", 4]
    let subs = find_array_element(inner, 0).ok_or(ParseError::MissingField("subscriptions"))?;

    // Parse elements 1 and 2 from a truncated view. We build a 2-tuple from
    // the remaining elements by finding element offsets.
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

pub fn parse_configure_result(body: &[u8]) -> Result<ConfigureResult<'_>, ParseError<'_>> {
    Ok(ConfigureResult(body))
}

/// Extract the negotiated version-rolling mask from a `mining.configure`
/// result body. Returns `None` when the pool did not advertise one — that
/// indicates either `version-rolling` was rejected or the pool uses the
/// older flat-object shape we don't support. Braiins and other public pools
/// serialise the mask as a hex string under the flat key
/// `"version-rolling.mask"`.
#[must_use]
pub fn parse_version_rolling_mask(body: &[u8]) -> Option<u32> {
    let value = find_field(body, b"version-rolling.mask")?;
    let hex = strip_json_string(value)?;
    parse_u32_hex(hex)
}

fn strip_json_string(value: &[u8]) -> Option<&[u8]> {
    if value.len() < 2 || value[0] != b'"' || value[value.len() - 1] != b'"' {
        return None;
    }
    Some(&value[1..value.len() - 1])
}

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

pub fn parse_boolean_result(body: &[u8]) -> Result<bool, ParseError<'_>> {
    let (value, _): (bool, _) = serde_json_core::from_slice(body).map_err(ParseError::Json)?;
    Ok(value)
}

// Returns a subslice of `json` that is the value of the named object field.
// Finds `"<key>":` and returns the bytes of the value token (object, array,
// string, or primitive) up to the next comma/brace at depth 0.
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

// Returns the bytes of the element at `target_index` in a JSON array that
// starts at `json[0] == '['`. Returns None when the array is shorter.
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

// Advances past a JSON value starting at `start` and returns the exclusive
// end index. Returns None on malformed input.
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

// Advances past a JSON string whose opening '"' is at `start`.
// Returns the exclusive end index (one past the closing '"').
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

fn is_ws(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\r' | b'\n')
}

fn is_null(value: &[u8]) -> bool {
    value == b"null"
}
