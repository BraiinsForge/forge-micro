//! JSON-RPC envelope types for Stratum V1.

use serde::{Deserialize, Serialize};

use crate::messages::{ClientReconnect, Notify, SetDifficulty, SetExtranonce, SetVersionMask};

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
