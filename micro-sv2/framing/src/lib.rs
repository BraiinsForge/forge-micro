//! Borrowed-first Stratum V2 frame structure for `no_std` environments.
//!
//! See `docs/superpowers/specs/2026-05-05-framing-design.md`.

#![no_std]
#![forbid(unsafe_code)]

#[cfg(test)]
extern crate std;

mod codec;
mod error;
mod frame;
mod header;
mod message;

pub use crate::error::FramingError;
pub use crate::frame::FrameView;
pub use crate::header::{ExtType, Header, MessageDescriptor, MsgType};
pub use crate::message::{Message, MessageGroup};
