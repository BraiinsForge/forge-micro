//! No-std Stratum V1 client parser
#![no_std]
// Error types are self-documenting via Debug; panic paths don't exist in this
// no-std/no-alloc crate so these docs add no value.
#![allow(clippy::missing_errors_doc, clippy::missing_panics_doc)]
// Spec-aligned names (ParseError, EncodeError, etc.) repeat the module name
// by design; renaming would diverge from the Stratum V1 terminology.
#![allow(clippy::module_name_repetitions)]

pub mod error;
pub mod line_buffer;
pub mod messages;
pub mod parse;
pub mod rpc;

pub use error::{EncodeError, ParseError};
pub use line_buffer::LineBuffer;
pub use messages::{
    encode_authorize, encode_configure, encode_extranonce_subscribe, encode_ping,
    encode_ping_response, encode_submit, encode_subscribe, Authorize, ClientReconnect, Configure,
    ConfigureResult, Notify, SetDifficulty, SetExtranonce, SetVersionMask, Submit, Subscribe,
    SubscribeResult,
};
pub use parse::{
    parse_boolean_result, parse_configure_result, parse_incoming, parse_subscribe_result,
};
pub use rpc::{Method, Notification, Response, Rpc, StratumError};
