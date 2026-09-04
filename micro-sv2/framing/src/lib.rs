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

//! Borrowed-first Stratum V2 frame structure for `no_std` environments.
//!
//! Holds the 6-byte frame [`Header`], the borrowed [`FrameView`] over a
//! complete frame, and the traits that connect messages to the wire:
//! [`Message`] carries a struct's static [`MessageDescriptor`],
//! [`MessageGroup`] dispatches a parsed frame into a typed enum variant
//! by its descriptor, and [`IntoFrame`] describes anything a codec can
//! encode. [`FrameCodec`] abstracts the wire transformation;
//! [`PlaintextCodec`] passes frames through unchanged, and the
//! encrypted codec in `ii-sv2-security` seals them with noise AEAD
//! chunks.

#![no_std]
#![forbid(unsafe_code)]

#[cfg(test)]
extern crate std;

mod codec;
mod error;
mod frame;
mod header;
mod message;

pub use crate::codec::{FrameCodec, PlaintextCodec};
pub use crate::error::FramingError;
pub use crate::frame::FrameView;
pub use crate::header::{ExtType, Header, MessageDescriptor, MsgType};
pub use crate::message::{IntoFrame, Message, MessageGroup, frame_to_message};
