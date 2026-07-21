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

//! JSON-RPC envelope types for Stratum V1.

use serde::{Deserialize, Serialize};

use super::messages::{ClientReconnect, Notify, SetDifficulty, SetExtranonce, SetVersionMask};

#[cfg(test)]
#[path = "tests/rpc.rs"]
mod tests;

// Notify<'a> contains heapless::Vec<&str, 32> which is inherently large.
// Boxing is not available in no_std/no_alloc context.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum Rpc<'a> {
    Response(Response<'a>),
    Notification(Notification<'a>),
}

#[derive(Debug)]
pub struct Response<'a> {
    pub id: u32,
    pub stratum_result: Option<&'a [u8]>,
    pub stratum_error: Option<StratumError<'a>>,
}

#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum Notification<'a> {
    Notify(Notify<'a>),
    SetDifficulty(SetDifficulty),
    SetVersionMask(SetVersionMask),
    SetExtranonce(SetExtranonce<'a>),
    // Pool-initiated keepalive. Carries an id so the client can reply
    // with the matching response via encode_ping_response.
    Ping { id: u32 },
    ClientReconnect(ClientReconnect<'a>),
}

#[derive(Debug)]
pub struct StratumError<'a>(pub i32, pub &'a str, pub Option<&'a str>);

#[derive(Serialize, Deserialize, Copy, Clone, PartialEq, Debug)]
pub enum Method {
    #[serde(rename = "mining.subscribe")]
    Subscribe,
    #[serde(rename = "mining.configure")]
    Configure,
    #[serde(rename = "mining.authorize")]
    Authorize,
    #[serde(rename = "mining.submit")]
    Submit,
    #[serde(rename = "mining.notify")]
    Notify,
    #[serde(rename = "mining.set_difficulty")]
    SetDifficulty,
    #[serde(rename = "mining.set_version_mask")]
    SetVersionMask,
    #[serde(rename = "mining.extranonce.subscribe")]
    ExtranonceSubscribe,
    #[serde(rename = "mining.set_extranonce")]
    SetExtranonce,
    #[serde(rename = "mining.ping")]
    Ping,
    #[serde(rename = "client.reconnect")]
    ClientReconnect,
}
