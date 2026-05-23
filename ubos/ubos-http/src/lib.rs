//! Edge-http accept-loop wrapper, HTTP response helpers, and the
//! `BufferWriter` slice-into-`Write` adapter.
//!
//! Handler crates implement `Service` and pass it to `serve`; the
//! accept loop takes care of binding, accepting, and driving the
//! edge-http state machine for each connection.
//!
//! `BufferWriter` is a `core::fmt::Write` adapter over a `&mut [u8]`
//! slice that tracks the write position and records overflow. All
//! response-building helpers use it to assemble headers and bodies
//! without heap allocation.

#![no_std]

pub mod buffer;
pub mod response;
pub mod serve;

pub use buffer::BufferWriter;
pub use core::net::IpAddr;
pub use response::{
    format_content_length, write_status_only, CONTENT_LENGTH_DECIMAL_DIGITS,
    MAX_EXTRA_STATUS_HEADERS,
};
pub use serve::{serve, Service};
