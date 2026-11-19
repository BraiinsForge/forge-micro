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

//! Edge-http accept-loop wrapper, HTTP response helpers, and the
//! `BufferWriter` slice-into-`Write` adapter.
//!
//! Handler crates implement `Service`. Board firmware calls `bind_tcp`
//! once to get a `'static` `TcpAccept`, then spawns one `serve_worker`
//! task per pool slot using the `http_worker_task!` macro. Per-worker
//! state lives in each task's own `TaskStorage` so a pool that cannot
//! fit fails the link step rather than crashing the board at runtime.
//!
//! `BufferWriter` is a `core::fmt::Write` adapter over a `&mut [u8]`
//! slice that tracks the write position and records overflow. All
//! response-building helpers use it to assemble headers and bodies
//! without heap allocation.

#![no_std]

pub mod buffer;
pub mod consts;
pub mod request;
pub mod response;
pub mod router;
pub mod serve;
pub mod status;

pub use buffer::BufferWriter;
pub use core::net::IpAddr;
// Re-exported so the `serve_http_pool!` and `http_worker_task!` macros can
// name these through `$crate::`. This keeps `edge-nal-embassy`,
// `static_cell`, and `embassy-time` internal to this crate instead of
// forcing every board that invokes the macros to declare them as direct
// dependencies.
pub use edge_nal_embassy::{Tcp, TcpAccept, TcpBuffers};
pub use embassy_time::Duration;
pub use request::{RequestParts, extract_request_parts};
pub use response::{
    CONTENT_LENGTH_DECIMAL_DIGITS, MAX_EXTRA_STATUS_HEADERS, MAX_RESPONSE_HEADERS,
    format_content_length, read_body, start_response, write_json_response, write_message,
    write_status_only,
};
pub use router::PrefixRouter;
pub use serve::{Service, bind_tcp, serve_worker, split_work_buffer};
pub use static_cell::{ConstStaticCell, StaticCell};
pub use status::{Status, StatusMessage};
