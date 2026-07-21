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

//! Stratum V1 wire message types and encoders.

use serde::Serialize;
use serde::de::{self, Deserializer, SeqAccess, Visitor};
use serde::ser::{SerializeSeq, Serializer};

use core::fmt;

#[cfg(test)]
#[path = "tests/messages.rs"]
mod tests;

// Upper bound on merkle branch length in mining.notify. 32 covers block
// heights well beyond current chain tip (2^32 txs per block).
pub const MAX_MERKLE_BRANCH_LEN: usize = 32;

#[derive(PartialEq)]
pub struct Notify<'a> {
    pub job_id: &'a str,
    pub prev_hash: &'a str,
    pub coin_base_1: &'a str,
    pub coin_base_2: &'a str,
    pub merkle_branch: heapless::Vec<&'a str, MAX_MERKLE_BRANCH_LEN>,
    pub version: &'a str,
    pub bits: &'a str,
    pub time: &'a str,
    pub clean_jobs: bool,
}

impl fmt::Debug for Notify<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Notify")
            .field("job_id", &self.job_id)
            .field("prev_hash", &self.prev_hash)
            .field("clean_jobs", &self.clean_jobs)
            .finish()
    }
}

// Notify arrives as a 9-element JSON array.
// Borrow all string slices directly from the input buffer.
impl<'de: 'a, 'a> serde::Deserialize<'de> for Notify<'a> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct NotifyVisitor;

        impl<'de> Visitor<'de> for NotifyVisitor {
            type Value = Notify<'de>;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(
                    "9-element array [job_id, prev_hash, cb1, cb2, branches, ver, bits, time, clean]",
                )
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let job_id: &str = seq
                    .next_element()?
                    .ok_or_else(|| de::Error::missing_field("job_id"))?;
                let prev_hash: &str = seq
                    .next_element()?
                    .ok_or_else(|| de::Error::missing_field("prev_hash"))?;
                let coin_base_1: &str = seq
                    .next_element()?
                    .ok_or_else(|| de::Error::missing_field("coin_base_1"))?;
                let coin_base_2: &str = seq
                    .next_element()?
                    .ok_or_else(|| de::Error::missing_field("coin_base_2"))?;
                let merkle_branch: heapless::Vec<&str, MAX_MERKLE_BRANCH_LEN> = seq
                    .next_element()?
                    .ok_or_else(|| de::Error::missing_field("merkle_branch"))?;
                let version: &str = seq
                    .next_element()?
                    .ok_or_else(|| de::Error::missing_field("version"))?;
                let bits: &str = seq
                    .next_element()?
                    .ok_or_else(|| de::Error::missing_field("bits"))?;
                let time: &str = seq
                    .next_element()?
                    .ok_or_else(|| de::Error::missing_field("time"))?;
                let clean_jobs: bool = seq
                    .next_element()?
                    .ok_or_else(|| de::Error::missing_field("clean_jobs"))?;
                Ok(Notify {
                    job_id,
                    prev_hash,
                    coin_base_1,
                    coin_base_2,
                    merkle_branch,
                    version,
                    bits,
                    time,
                    clean_jobs,
                })
            }
        }

        deserializer.deserialize_seq(NotifyVisitor)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SetDifficulty(pub f64);

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SetVersionMask {
    pub mask: u32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SetExtranonce<'a> {
    pub extra_nonce_1: &'a str,
    pub extra_nonce_2_size: u8,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClientReconnect<'a> {
    pub host: &'a str,
    pub port: u16,
    pub wait: u16,
}

#[derive(Debug)]
pub struct Subscribe<'a> {
    pub agent_signature: Option<&'a str>,
    pub extra_nonce_1: Option<&'a str>,
}

#[derive(Debug)]
pub struct Configure<'a> {
    // Names of requested extensions, e.g. &["version-rolling"].
    pub features: &'a [&'a str],
    // Pre-encoded JSON object with extension params. Embedded verbatim.
    pub configure_map: &'a [u8],
}

#[derive(Debug)]
pub struct Authorize<'a> {
    pub name: &'a str,
    pub password: &'a str,
}

#[derive(Debug)]
pub struct Submit<'a> {
    pub user_name: &'a str,
    pub job_id: &'a str,
    pub extra_nonce_2: &'a str,
    pub time: &'a str,
    pub nonce: &'a str,
    pub version: &'a str,
}

pub struct SubscribeResult<'a>(
    pub &'a [u8], // subscriptions — raw JSON slice
    pub &'a str,  // extra_nonce_1 (hex)
    pub u8,       // extra_nonce_2_size
);

impl fmt::Debug for SubscribeResult<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("SubscribeResult")
            .field(&self.1)
            .field(&self.2)
            .finish()
    }
}

pub struct ConfigureResult<'a>(pub &'a [u8]);

impl fmt::Debug for ConfigureResult<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("ConfigureResult").finish()
    }
}

#[derive(Serialize)]
struct RequestEnvelope<P: Serialize> {
    id: u32,
    method: &'static str,
    params: P,
}

struct SubscribeParams<'a>(&'a Subscribe<'a>);

impl Serialize for SubscribeParams<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut seq = s.serialize_seq(Some(2))?;
        seq.serialize_element(&self.0.agent_signature)?;
        seq.serialize_element(&self.0.extra_nonce_1)?;
        seq.end()
    }
}

struct AuthorizeParams<'a>(&'a Authorize<'a>);

impl Serialize for AuthorizeParams<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut seq = s.serialize_seq(Some(2))?;
        seq.serialize_element(self.0.name)?;
        seq.serialize_element(self.0.password)?;
        seq.end()
    }
}

struct SubmitParams<'a>(&'a Submit<'a>);

impl Serialize for SubmitParams<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut seq = s.serialize_seq(Some(6))?;
        seq.serialize_element(self.0.user_name)?;
        seq.serialize_element(self.0.job_id)?;
        seq.serialize_element(self.0.extra_nonce_2)?;
        seq.serialize_element(self.0.time)?;
        seq.serialize_element(self.0.nonce)?;
        seq.serialize_element(self.0.version)?;
        seq.end()
    }
}

pub fn encode_subscribe(
    buf: &mut [u8],
    id: u32,
    msg: &Subscribe<'_>,
) -> Result<usize, crate::error::EncodeError> {
    let req = RequestEnvelope {
        id,
        method: "mining.subscribe",
        params: SubscribeParams(msg),
    };
    encode_with_serde(buf, &req)
}

pub fn encode_authorize(
    buf: &mut [u8],
    id: u32,
    msg: &Authorize<'_>,
) -> Result<usize, crate::error::EncodeError> {
    let req = RequestEnvelope {
        id,
        method: "mining.authorize",
        params: AuthorizeParams(msg),
    };
    encode_with_serde(buf, &req)
}

pub fn encode_submit(
    buf: &mut [u8],
    id: u32,
    msg: &Submit<'_>,
) -> Result<usize, crate::error::EncodeError> {
    let req = RequestEnvelope {
        id,
        method: "mining.submit",
        params: SubmitParams(msg),
    };
    encode_with_serde(buf, &req)
}

pub fn encode_extranonce_subscribe(
    buf: &mut [u8],
    id: u32,
) -> Result<usize, crate::error::EncodeError> {
    let req = RequestEnvelope::<[&str; 0]> {
        id,
        method: "mining.extranonce.subscribe",
        params: [],
    };
    encode_with_serde(buf, &req)
}

pub fn encode_ping(buf: &mut [u8], id: u32) -> Result<usize, crate::error::EncodeError> {
    let req = RequestEnvelope::<[&str; 0]> {
        id,
        method: "mining.ping",
        params: [],
    };
    encode_with_serde(buf, &req)
}

// Empty reply to a pool-initiated mining.ping: {"id":N,"result":null,"error":null}
pub fn encode_ping_response(buf: &mut [u8], id: u32) -> Result<usize, crate::error::EncodeError> {
    let mut w = BufWriter::new(buf);
    w.emit(br#"{"id":"#)?;
    w.emit_u32(id)?;
    w.emit(br#","result":null,"error":null}"#)?;
    w.emit(b"\n")?;
    Ok(w.pos)
}

// encode_configure writes the envelope manually. configure_map must be a
// valid JSON object; it is embedded verbatim, not quoted. serde-json-core
// has no raw-value serialisation path so we bypass serde here.
pub fn encode_configure(
    buf: &mut [u8],
    id: u32,
    msg: &Configure<'_>,
) -> Result<usize, crate::error::EncodeError> {
    let mut w = BufWriter::new(buf);
    w.emit(br#"{"id":"#)?;
    w.emit_u32(id)?;
    w.emit(br#","method":"mining.configure","params":["#)?;
    w.emit(b"[")?;
    for (index, feat) in msg.features.iter().enumerate() {
        if index > 0 {
            w.emit(b",")?;
        }
        w.emit(b"\"")?;
        w.emit(feat.as_bytes())?;
        w.emit(b"\"")?;
    }
    w.emit(b"],")?;
    w.emit(msg.configure_map)?;
    w.emit(b"]}\n")?;
    Ok(w.pos)
}

fn encode_with_serde<T: Serialize>(
    buf: &mut [u8],
    value: &T,
) -> Result<usize, crate::error::EncodeError> {
    let n =
        serde_json_core::to_slice(value, buf).map_err(|_| crate::error::EncodeError::BufferFull)?;
    if n >= buf.len() {
        return Err(crate::error::EncodeError::BufferFull);
    }
    buf[n] = b'\n';
    Ok(n + 1)
}

struct BufWriter<'b> {
    buf: &'b mut [u8],
    pos: usize,
}

impl<'b> BufWriter<'b> {
    fn new(buf: &'b mut [u8]) -> Self {
        BufWriter { buf, pos: 0 }
    }

    fn emit(&mut self, data: &[u8]) -> Result<(), crate::error::EncodeError> {
        let end = self.pos + data.len();
        if end > self.buf.len() {
            return Err(crate::error::EncodeError::BufferFull);
        }
        self.buf[self.pos..end].copy_from_slice(data);
        self.pos = end;
        Ok(())
    }

    fn emit_u32(&mut self, mut value: u32) -> Result<(), crate::error::EncodeError> {
        // u32::MAX is 4_294_967_295 — ten decimal digits, no sign.
        const MAX_U32_DECIMAL_DIGITS: usize = 10;
        let mut digits = [0u8; MAX_U32_DECIMAL_DIGITS];
        let mut len = 0;
        if value == 0 {
            return self.emit(b"0");
        }
        while value > 0 {
            // value % 10 is always 0-9, so the cast to u8 cannot truncate.
            #[allow(clippy::cast_possible_truncation)]
            let digit = b'0' + (value % 10) as u8;
            digits[len] = digit;
            value /= 10;
            len += 1;
        }
        digits[..len].reverse();
        self.emit(&digits[..len])
    }
}
