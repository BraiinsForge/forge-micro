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

//! Stratum V2 elementary mining messages — borrowed-first, `no_std`.
//!
//! One struct per wire message for connection setup
//! ([`SetupConnection`] and its replies), standard-channel opening
//! ([`OpenStandardMiningChannel`] and its replies), job distribution
//! ([`NewMiningJob`], [`SetNewPrevHash`], [`SetTarget`]), and share
//! submission ([`SubmitSharesStandard`] and its replies). Each
//! implements `Encode`/`Decode` plus the `Message` trait with its
//! static descriptor, and [`MiningMessages`] is the `MessageGroup`
//! enum that dispatches a parsed frame into the right variant.

#![no_std]
#![forbid(unsafe_code)]

#[cfg(test)]
extern crate std;

use ii_sv2_framing::ExtType;

/// SV2 extension byte for the basic mining protocol.
pub const MINING_EXTENSION_TYPE: ExtType = 0x00;

use ii_sv2_types::{Decode, Encode, Error as TypeError, Str0_255};

/// Telemetry the client provides to the server during connection setup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DeviceInfo<'a> {
    /// E.g. "Bitmain"
    pub vendor: Str0_255<'a>,
    /// E.g. "S9i 13.5"
    pub hw_rev: Str0_255<'a>,
    /// E.g. "braiins-os-2018-09-22-1-hash"
    pub fw_ver: Str0_255<'a>,
    /// Vendor-defined unique device id; clients may set to "" for telemetry-off.
    pub dev_id: Str0_255<'a>,
}

impl<'a> Encode for DeviceInfo<'a> {
    fn encoded_len(&self) -> usize {
        self.vendor.encoded_len()
            + self.hw_rev.encoded_len()
            + self.fw_ver.encoded_len()
            + self.dev_id.encoded_len()
    }

    fn encode_to(&self, out: &mut [u8]) -> Result<usize, TypeError> {
        let mut n = 0;
        n += self.vendor.encode_to(&mut out[n..])?;
        n += self.hw_rev.encode_to(&mut out[n..])?;
        n += self.fw_ver.encode_to(&mut out[n..])?;
        n += self.dev_id.encode_to(&mut out[n..])?;
        Ok(n)
    }
}

impl<'de> Decode<'de> for DeviceInfo<'de> {
    fn decode(input: &mut &'de [u8]) -> Result<Self, TypeError> {
        Ok(Self {
            vendor: Str0_255::decode(input)?,
            hw_rev: Str0_255::decode(input)?,
            fw_ver: Str0_255::decode(input)?,
            dev_id: Str0_255::decode(input)?,
        })
    }
}

use ii_sv2_framing::{Message, MessageDescriptor};

/// Initiates the connection. MUST be the first message sent by the client.
/// Server replies with `SetupConnectionSuccess` or `SetupConnectionError`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SetupConnection<'a> {
    /// 0 = Mining, 1 = Job Negotiation, 2 = Template Distribution, 3 = Job Distribution.
    pub protocol: u8,
    pub min_version: u16,
    pub max_version: u16,
    /// Bitfield. See associated constants below.
    pub flags: u32,
    pub endpoint_host: Str0_255<'a>,
    pub endpoint_port: u16,
    pub device: DeviceInfo<'a>,
}

impl<'a> SetupConnection<'a> {
    pub const REQUIRES_STANDARD_JOBS: u32 = 1;
    pub const REQUIRES_WORK_SELECTION: u32 = 1 << 1;
    pub const REQUIRES_VERSION_ROLLING: u32 = 1 << 2;
}

impl<'a> Encode for SetupConnection<'a> {
    fn encoded_len(&self) -> usize {
        self.protocol.encoded_len()
            + self.min_version.encoded_len()
            + self.max_version.encoded_len()
            + self.flags.encoded_len()
            + self.endpoint_host.encoded_len()
            + self.endpoint_port.encoded_len()
            + self.device.encoded_len()
    }

    fn encode_to(&self, out: &mut [u8]) -> Result<usize, TypeError> {
        let mut n = 0;
        n += self.protocol.encode_to(&mut out[n..])?;
        n += self.min_version.encode_to(&mut out[n..])?;
        n += self.max_version.encode_to(&mut out[n..])?;
        n += self.flags.encode_to(&mut out[n..])?;
        n += self.endpoint_host.encode_to(&mut out[n..])?;
        n += self.endpoint_port.encode_to(&mut out[n..])?;
        n += self.device.encode_to(&mut out[n..])?;
        Ok(n)
    }
}

impl<'de> Decode<'de> for SetupConnection<'de> {
    fn decode(input: &mut &'de [u8]) -> Result<Self, TypeError> {
        Ok(Self {
            protocol: u8::decode(input)?,
            min_version: u16::decode(input)?,
            max_version: u16::decode(input)?,
            flags: u32::decode(input)?,
            endpoint_host: Str0_255::decode(input)?,
            endpoint_port: u16::decode(input)?,
            device: DeviceInfo::decode(input)?,
        })
    }
}

impl<'a> Message for SetupConnection<'a> {
    const DESCRIPTOR: MessageDescriptor =
        MessageDescriptor::new(MINING_EXTENSION_TYPE, 0x00, false);
}

/// Server's positive acknowledgement of a `SetupConnection`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SetupConnectionSuccess {
    pub used_version: u16,
    pub flags: u32,
}

impl SetupConnectionSuccess {
    /// The upstream accepts no changes to the header's version field.
    /// Absent, the downstream may roll the BIP320 bits: the success
    /// flags carry the upstream's own requirements and are not an echo
    /// of what the downstream asked for.
    pub const REQUIRES_FIXED_VERSION: u32 = 1;
}

impl Encode for SetupConnectionSuccess {
    fn encoded_len(&self) -> usize {
        self.used_version.encoded_len() + self.flags.encoded_len()
    }

    fn encode_to(&self, out: &mut [u8]) -> Result<usize, TypeError> {
        let mut n = 0;
        n += self.used_version.encode_to(&mut out[n..])?;
        n += self.flags.encode_to(&mut out[n..])?;
        Ok(n)
    }
}

impl<'de> Decode<'de> for SetupConnectionSuccess {
    fn decode(input: &mut &'de [u8]) -> Result<Self, TypeError> {
        Ok(Self {
            used_version: u16::decode(input)?,
            flags: u32::decode(input)?,
        })
    }
}

impl Message for SetupConnectionSuccess {
    const DESCRIPTOR: MessageDescriptor =
        MessageDescriptor::new(MINING_EXTENSION_TYPE, 0x01, false);
}

/// Server's negative acknowledgement of a `SetupConnection`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SetupConnectionError<'a> {
    pub flags: u32,
    pub code: Str0_255<'a>,
}

impl<'a> Encode for SetupConnectionError<'a> {
    fn encoded_len(&self) -> usize {
        self.flags.encoded_len() + self.code.encoded_len()
    }

    fn encode_to(&self, out: &mut [u8]) -> Result<usize, TypeError> {
        let mut n = 0;
        n += self.flags.encode_to(&mut out[n..])?;
        n += self.code.encode_to(&mut out[n..])?;
        Ok(n)
    }
}

impl<'de> Decode<'de> for SetupConnectionError<'de> {
    fn decode(input: &mut &'de [u8]) -> Result<Self, TypeError> {
        Ok(Self {
            flags: u32::decode(input)?,
            code: Str0_255::decode(input)?,
        })
    }
}

impl<'a> Message for SetupConnectionError<'a> {
    const DESCRIPTOR: MessageDescriptor =
        MessageDescriptor::new(MINING_EXTENSION_TYPE, 0x02, false);
}

use ii_sv2_types::Uint256;

/// Client requests a standard mining channel from the upstream node.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OpenStandardMiningChannel<'a> {
    pub request_id: u32,
    /// Worker / pool identity (e.g. "user.worker1").
    pub user: Str0_255<'a>,
    /// Expected hashrate in h/s. The protocol puts it on the wire as an
    /// IEEE-754 single, which the codec narrows to and widens from.
    /// Proxies with no devices yet send 0.0.
    pub nominal_hashrate: f64,
    pub max_target: Uint256<'a>,
}

impl<'a> Encode for OpenStandardMiningChannel<'a> {
    fn encoded_len(&self) -> usize {
        self.request_id.encoded_len()
            + self.user.encoded_len()
            + (self.nominal_hashrate as f32).encoded_len()
            + self.max_target.encoded_len()
    }

    fn encode_to(&self, out: &mut [u8]) -> Result<usize, TypeError> {
        let mut n = 0;
        n += self.request_id.encode_to(&mut out[n..])?;
        n += self.user.encode_to(&mut out[n..])?;
        n += (self.nominal_hashrate as f32).encode_to(&mut out[n..])?;
        n += self.max_target.encode_to(&mut out[n..])?;
        Ok(n)
    }
}

impl<'de> Decode<'de> for OpenStandardMiningChannel<'de> {
    fn decode(input: &mut &'de [u8]) -> Result<Self, TypeError> {
        Ok(Self {
            request_id: u32::decode(input)?,
            user: Str0_255::decode(input)?,
            nominal_hashrate: f64::from(f32::decode(input)?),
            max_target: Uint256::decode(input)?,
        })
    }
}

impl<'a> Message for OpenStandardMiningChannel<'a> {
    const DESCRIPTOR: MessageDescriptor =
        MessageDescriptor::new(MINING_EXTENSION_TYPE, 0x10, false);
}

use ii_sv2_types::Bytes0_32;

/// Server's positive response to `OpenStandardMiningChannel`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct OpenStandardMiningChannelSuccess<'a> {
    pub request_id: u32,
    pub channel_id: u32,
    pub target: Uint256<'a>,
    pub extranonce_prefix: Bytes0_32<'a>,
    /// Group channel into which the new channel belongs.
    pub group_channel_id: u32,
}

impl<'a> Encode for OpenStandardMiningChannelSuccess<'a> {
    fn encoded_len(&self) -> usize {
        self.request_id.encoded_len()
            + self.channel_id.encoded_len()
            + self.target.encoded_len()
            + self.extranonce_prefix.encoded_len()
            + self.group_channel_id.encoded_len()
    }

    fn encode_to(&self, out: &mut [u8]) -> Result<usize, TypeError> {
        let mut n = 0;
        n += self.request_id.encode_to(&mut out[n..])?;
        n += self.channel_id.encode_to(&mut out[n..])?;
        n += self.target.encode_to(&mut out[n..])?;
        n += self.extranonce_prefix.encode_to(&mut out[n..])?;
        n += self.group_channel_id.encode_to(&mut out[n..])?;
        Ok(n)
    }
}

impl<'de> Decode<'de> for OpenStandardMiningChannelSuccess<'de> {
    fn decode(input: &mut &'de [u8]) -> Result<Self, TypeError> {
        Ok(Self {
            request_id: u32::decode(input)?,
            channel_id: u32::decode(input)?,
            target: Uint256::decode(input)?,
            extranonce_prefix: Bytes0_32::decode(input)?,
            group_channel_id: u32::decode(input)?,
        })
    }
}

impl<'a> Message for OpenStandardMiningChannelSuccess<'a> {
    const DESCRIPTOR: MessageDescriptor =
        MessageDescriptor::new(MINING_EXTENSION_TYPE, 0x11, false);
}

/// Server's negative response to `OpenStandardMiningChannel`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct OpenMiningChannelError<'a> {
    pub request_id: u32,
    pub code: Str0_255<'a>,
}

impl<'a> OpenMiningChannelError<'a> {
    pub const UNKNOWN_USER: &'static str = "unknown-user";
    pub const TOO_MANY_CHANNELS: &'static str = "too-many-channels";
    pub const DUPLICATED_REQUEST_ID: &'static str = "duplicate-request_id";
    pub const TOO_LARGE_EXTRANONCE: &'static str = "too-large-extranonce-requested";
}

impl<'a> Encode for OpenMiningChannelError<'a> {
    fn encoded_len(&self) -> usize {
        self.request_id.encoded_len() + self.code.encoded_len()
    }

    fn encode_to(&self, out: &mut [u8]) -> Result<usize, TypeError> {
        let mut n = 0;
        n += self.request_id.encode_to(&mut out[n..])?;
        n += self.code.encode_to(&mut out[n..])?;
        Ok(n)
    }
}

impl<'de> Decode<'de> for OpenMiningChannelError<'de> {
    fn decode(input: &mut &'de [u8]) -> Result<Self, TypeError> {
        Ok(Self {
            request_id: u32::decode(input)?,
            code: Str0_255::decode(input)?,
        })
    }
}

impl<'a> Message for OpenMiningChannelError<'a> {
    const DESCRIPTOR: MessageDescriptor =
        MessageDescriptor::new(MINING_EXTENSION_TYPE, 0x12, false);
}

/// Server provides a new mining job to the client through a standard channel.
///
/// `min_ntime`: `Some(t)` activates immediately at `min_ntime = t`; `None`
/// indicates a future job awaiting a matching `SetNewPrevHash`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NewMiningJob<'a> {
    pub channel_id: u32,
    pub job_id: u32,
    pub min_ntime: Option<u32>,
    pub version: u32,
    pub merkle_root: Uint256<'a>,
}

impl<'a> Encode for NewMiningJob<'a> {
    fn encoded_len(&self) -> usize {
        self.channel_id.encoded_len()
            + self.job_id.encoded_len()
            + self.min_ntime.encoded_len()
            + self.version.encoded_len()
            + self.merkle_root.encoded_len()
    }

    fn encode_to(&self, out: &mut [u8]) -> Result<usize, TypeError> {
        let mut n = 0;
        n += self.channel_id.encode_to(&mut out[n..])?;
        n += self.job_id.encode_to(&mut out[n..])?;
        n += self.min_ntime.encode_to(&mut out[n..])?;
        n += self.version.encode_to(&mut out[n..])?;
        n += self.merkle_root.encode_to(&mut out[n..])?;
        Ok(n)
    }
}

impl<'de> Decode<'de> for NewMiningJob<'de> {
    fn decode(input: &mut &'de [u8]) -> Result<Self, TypeError> {
        Ok(Self {
            channel_id: u32::decode(input)?,
            job_id: u32::decode(input)?,
            min_ntime: <Option<u32>>::decode(input)?,
            version: u32::decode(input)?,
            merkle_root: Uint256::decode(input)?,
        })
    }
}

impl<'a> Message for NewMiningJob<'a> {
    const DESCRIPTOR: MessageDescriptor = MessageDescriptor::new(MINING_EXTENSION_TYPE, 0x15, true);
}

/// Distributed when a new block is detected upstream. Clients MUST
/// immediately mine on the provided prevhash + referenced job id;
/// queued jobs become invalid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SetNewPrevHash<'a> {
    pub channel_id: u32,
    pub job_id: u32,
    pub prev_hash: Uint256<'a>,
    pub min_ntime: u32,
    pub nbits: u32,
}

impl<'a> Encode for SetNewPrevHash<'a> {
    fn encoded_len(&self) -> usize {
        self.channel_id.encoded_len()
            + self.job_id.encoded_len()
            + self.prev_hash.encoded_len()
            + self.min_ntime.encoded_len()
            + self.nbits.encoded_len()
    }

    fn encode_to(&self, out: &mut [u8]) -> Result<usize, TypeError> {
        let mut n = 0;
        n += self.channel_id.encode_to(&mut out[n..])?;
        n += self.job_id.encode_to(&mut out[n..])?;
        n += self.prev_hash.encode_to(&mut out[n..])?;
        n += self.min_ntime.encode_to(&mut out[n..])?;
        n += self.nbits.encode_to(&mut out[n..])?;
        Ok(n)
    }
}

impl<'de> Decode<'de> for SetNewPrevHash<'de> {
    fn decode(input: &mut &'de [u8]) -> Result<Self, TypeError> {
        Ok(Self {
            channel_id: u32::decode(input)?,
            job_id: u32::decode(input)?,
            prev_hash: Uint256::decode(input)?,
            min_ntime: u32::decode(input)?,
            nbits: u32::decode(input)?,
        })
    }
}

impl<'a> Message for SetNewPrevHash<'a> {
    const DESCRIPTOR: MessageDescriptor = MessageDescriptor::new(MINING_EXTENSION_TYPE, 0x20, true);
}

/// Server adjusts the difficulty target on a channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SetTarget<'a> {
    pub channel_id: u32,
    pub max_target: Uint256<'a>,
}

impl<'a> Encode for SetTarget<'a> {
    fn encoded_len(&self) -> usize {
        self.channel_id.encoded_len() + self.max_target.encoded_len()
    }

    fn encode_to(&self, out: &mut [u8]) -> Result<usize, TypeError> {
        let mut n = 0;
        n += self.channel_id.encode_to(&mut out[n..])?;
        n += self.max_target.encode_to(&mut out[n..])?;
        Ok(n)
    }
}

impl<'de> Decode<'de> for SetTarget<'de> {
    fn decode(input: &mut &'de [u8]) -> Result<Self, TypeError> {
        Ok(Self {
            channel_id: u32::decode(input)?,
            max_target: Uint256::decode(input)?,
        })
    }
}

impl<'a> Message for SetTarget<'a> {
    const DESCRIPTOR: MessageDescriptor = MessageDescriptor::new(MINING_EXTENSION_TYPE, 0x21, true);
}

/// Client submits a found share on a standard channel. Wire form is
/// 24 fixed bytes (six u32 LE fields) — the smallest mining message
/// in the protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SubmitSharesStandard {
    pub channel_id: u32,
    /// Per-channel monotonic counter; the server echoes the highest
    /// observed value back in `SubmitSharesSuccess.last_sequence_number`.
    pub sequence_number: u32,
    pub job_id: u32,
    pub nonce: u32,
    pub ntime: u32,
    pub version: u32,
}

impl Encode for SubmitSharesStandard {
    fn encoded_len(&self) -> usize {
        self.channel_id.encoded_len()
            + self.sequence_number.encoded_len()
            + self.job_id.encoded_len()
            + self.nonce.encoded_len()
            + self.ntime.encoded_len()
            + self.version.encoded_len()
    }

    fn encode_to(&self, out: &mut [u8]) -> Result<usize, TypeError> {
        let mut n = 0;
        n += self.channel_id.encode_to(&mut out[n..])?;
        n += self.sequence_number.encode_to(&mut out[n..])?;
        n += self.job_id.encode_to(&mut out[n..])?;
        n += self.nonce.encode_to(&mut out[n..])?;
        n += self.ntime.encode_to(&mut out[n..])?;
        n += self.version.encode_to(&mut out[n..])?;
        Ok(n)
    }
}

impl<'de> Decode<'de> for SubmitSharesStandard {
    fn decode(input: &mut &'de [u8]) -> Result<Self, TypeError> {
        Ok(Self {
            channel_id: u32::decode(input)?,
            sequence_number: u32::decode(input)?,
            job_id: u32::decode(input)?,
            nonce: u32::decode(input)?,
            ntime: u32::decode(input)?,
            version: u32::decode(input)?,
        })
    }
}

impl Message for SubmitSharesStandard {
    const DESCRIPTOR: MessageDescriptor = MessageDescriptor::new(MINING_EXTENSION_TYPE, 0x1a, true);
}

/// Server's positive ack covering one or more `SubmitSharesStandard`
/// frames. `new_shares_sum` is the cumulative diff-share count
/// added since the last success/error pair on this channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SubmitSharesSuccess {
    pub channel_id: u32,
    pub last_sequence_number: u32,
    pub new_submits_accepted_count: u32,
    pub new_shares_sum: u64,
}

impl Encode for SubmitSharesSuccess {
    fn encoded_len(&self) -> usize {
        self.channel_id.encoded_len()
            + self.last_sequence_number.encoded_len()
            + self.new_submits_accepted_count.encoded_len()
            + self.new_shares_sum.encoded_len()
    }

    fn encode_to(&self, out: &mut [u8]) -> Result<usize, TypeError> {
        let mut n = 0;
        n += self.channel_id.encode_to(&mut out[n..])?;
        n += self.last_sequence_number.encode_to(&mut out[n..])?;
        n += self.new_submits_accepted_count.encode_to(&mut out[n..])?;
        n += self.new_shares_sum.encode_to(&mut out[n..])?;
        Ok(n)
    }
}

impl<'de> Decode<'de> for SubmitSharesSuccess {
    fn decode(input: &mut &'de [u8]) -> Result<Self, TypeError> {
        Ok(Self {
            channel_id: u32::decode(input)?,
            last_sequence_number: u32::decode(input)?,
            new_submits_accepted_count: u32::decode(input)?,
            new_shares_sum: u64::decode(input)?,
        })
    }
}

impl Message for SubmitSharesSuccess {
    const DESCRIPTOR: MessageDescriptor = MessageDescriptor::new(MINING_EXTENSION_TYPE, 0x1c, true);
}

/// Server's negative ack for a previously submitted share.
/// `error_code` is one of the named constants below per SV2 spec;
/// unknown codes propagate as-is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SubmitSharesError<'a> {
    pub channel_id: u32,
    pub sequence_number: u32,
    pub error_code: Str0_255<'a>,
}

impl<'a> SubmitSharesError<'a> {
    pub const INVALID_CHANNEL_ID: &'static str = "invalid-channel-id";
    pub const STALE_SHARE: &'static str = "stale-share";
    pub const DIFFICULTY_TOO_LOW: &'static str = "difficulty-too-low";
    pub const INVALID_JOB_ID: &'static str = "invalid-job-id";
}

impl<'a> Encode for SubmitSharesError<'a> {
    fn encoded_len(&self) -> usize {
        self.channel_id.encoded_len()
            + self.sequence_number.encoded_len()
            + self.error_code.encoded_len()
    }

    fn encode_to(&self, out: &mut [u8]) -> Result<usize, TypeError> {
        let mut n = 0;
        n += self.channel_id.encode_to(&mut out[n..])?;
        n += self.sequence_number.encode_to(&mut out[n..])?;
        n += self.error_code.encode_to(&mut out[n..])?;
        Ok(n)
    }
}

impl<'de> Decode<'de> for SubmitSharesError<'de> {
    fn decode(input: &mut &'de [u8]) -> Result<Self, TypeError> {
        Ok(Self {
            channel_id: u32::decode(input)?,
            sequence_number: u32::decode(input)?,
            error_code: Str0_255::decode(input)?,
        })
    }
}

impl<'a> Message for SubmitSharesError<'a> {
    const DESCRIPTOR: MessageDescriptor = MessageDescriptor::new(MINING_EXTENSION_TYPE, 0x1d, true);
}

use ii_sv2_framing::{FrameView, FramingError, MessageGroup, frame_to_message};

/// Dispatcher enum covering every elementary mining message recognized by
/// this crate.
///
/// Note: no `Eq`/`Hash` because `OpenStandardMiningChannel` carries an
/// `f64` field.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MiningMessages<'a> {
    SetupConnection(SetupConnection<'a>),
    SetupConnectionSuccess(SetupConnectionSuccess),
    SetupConnectionError(SetupConnectionError<'a>),
    OpenStandardMiningChannel(OpenStandardMiningChannel<'a>),
    OpenStandardMiningChannelSuccess(OpenStandardMiningChannelSuccess<'a>),
    OpenMiningChannelError(OpenMiningChannelError<'a>),
    NewMiningJob(NewMiningJob<'a>),
    SetNewPrevHash(SetNewPrevHash<'a>),
    SetTarget(SetTarget<'a>),
    SubmitSharesStandard(SubmitSharesStandard),
    SubmitSharesSuccess(SubmitSharesSuccess),
    SubmitSharesError(SubmitSharesError<'a>),
}

impl<'de> MessageGroup<'de> for MiningMessages<'de> {
    const EXTENSION_TYPE: ExtType = MINING_EXTENSION_TYPE;

    fn dispatch(frame: FrameView<'de>) -> Result<Self, FramingError> {
        if frame.header.extension_type != Self::EXTENSION_TYPE {
            return Err(FramingError::WrongExtension {
                expected: Self::EXTENSION_TYPE,
                found: frame.header.extension_type,
            });
        }
        match (frame.header.msg_type, frame.header.is_channel_message) {
            (0x00, false) => Ok(MiningMessages::SetupConnection(frame_to_message(frame)?)),
            (0x01, false) => Ok(MiningMessages::SetupConnectionSuccess(frame_to_message(
                frame,
            )?)),
            (0x02, false) => Ok(MiningMessages::SetupConnectionError(frame_to_message(
                frame,
            )?)),
            (0x10, false) => Ok(MiningMessages::OpenStandardMiningChannel(frame_to_message(
                frame,
            )?)),
            (0x11, false) => Ok(MiningMessages::OpenStandardMiningChannelSuccess(
                frame_to_message(frame)?,
            )),
            (0x12, false) => Ok(MiningMessages::OpenMiningChannelError(frame_to_message(
                frame,
            )?)),
            (0x15, true) => Ok(MiningMessages::NewMiningJob(frame_to_message(frame)?)),
            (0x1a, true) => Ok(MiningMessages::SubmitSharesStandard(frame_to_message(
                frame,
            )?)),
            (0x1c, true) => Ok(MiningMessages::SubmitSharesSuccess(frame_to_message(
                frame,
            )?)),
            (0x1d, true) => Ok(MiningMessages::SubmitSharesError(frame_to_message(frame)?)),
            (0x20, true) => Ok(MiningMessages::SetNewPrevHash(frame_to_message(frame)?)),
            (0x21, true) => Ok(MiningMessages::SetTarget(frame_to_message(frame)?)),
            _ => Err(FramingError::UnknownMessage(frame.header.descriptor())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ii_sv2_types::Uint256;
    use ii_sv2_types::{Decode, Encode, Str0_255};

    const TARGET_PATTERN: [u8; 32] = [
        0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
        0x0f, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d,
        0x1e, 0x1f,
    ];

    #[test]
    fn open_standard_mining_channel_roundtrip() {
        let original = OpenStandardMiningChannel {
            request_id: 1,
            user: Str0_255::try_from("user.worker1").unwrap(),
            // 2^40 h/s, which the single on the wire holds exactly.
            nominal_hashrate: 1_099_511_627_776.0,
            max_target: Uint256::from_le_bytes(&TARGET_PATTERN),
        };
        let mut codec = PlaintextCodec;
        let mut wire = [0u8; 128];
        let _n = codec.encode(&original, &mut wire).unwrap();

        let mut hbuf = [0u8; PlaintextCodec::HEADER_WIRE_SIZE];
        hbuf.copy_from_slice(&wire[..PlaintextCodec::HEADER_WIRE_SIZE]);
        let header = codec.decode_header(&mut hbuf).unwrap();
        let pn = PlaintextCodec::payload_wire_size(header.msg_length as usize);
        let mut pbuf = [0u8; 128];
        pbuf[..pn].copy_from_slice(
            &wire[PlaintextCodec::HEADER_WIRE_SIZE..PlaintextCodec::HEADER_WIRE_SIZE + pn],
        );
        let frame = codec.decode_frame(header, &mut pbuf[..pn]).unwrap();
        let decoded: OpenStandardMiningChannel<'_> = frame_to_message(frame).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn device_info_roundtrip() {
        let original = DeviceInfo {
            vendor: Str0_255::try_from("Bitmain").unwrap(),
            hw_rev: Str0_255::try_from("S9i 13.5").unwrap(),
            fw_ver: Str0_255::try_from("braiins-os-2018").unwrap(),
            dev_id: Str0_255::try_from("dev-001").unwrap(),
        };
        let mut buf = [0u8; 64];
        let n = original.encode_to(&mut buf).unwrap();
        let mut cursor: &[u8] = &buf[..n];
        let decoded = DeviceInfo::decode(&mut cursor).unwrap();
        assert_eq!(decoded, original);
        assert!(cursor.is_empty());
    }

    use ii_sv2_framing::{FrameCodec, PlaintextCodec, frame_to_message};

    #[test]
    fn setup_connection_roundtrip() {
        let original = SetupConnection {
            protocol: 0,
            min_version: 2,
            max_version: 2,
            flags: SetupConnection::REQUIRES_VERSION_ROLLING,
            endpoint_host: Str0_255::try_from("pool.example.com").unwrap(),
            endpoint_port: 3333,
            device: DeviceInfo {
                vendor: Str0_255::try_from("Bitmain").unwrap(),
                hw_rev: Str0_255::try_from("S9i").unwrap(),
                fw_ver: Str0_255::try_from("bos-2018").unwrap(),
                dev_id: Str0_255::try_from("d1").unwrap(),
            },
        };

        let mut codec = PlaintextCodec;
        let mut wire = [0u8; 256];
        let _n = codec.encode(&original, &mut wire).unwrap();

        let mut hbuf = [0u8; PlaintextCodec::HEADER_WIRE_SIZE];
        hbuf.copy_from_slice(&wire[..PlaintextCodec::HEADER_WIRE_SIZE]);
        let header = codec.decode_header(&mut hbuf).unwrap();
        let pn = PlaintextCodec::payload_wire_size(header.msg_length as usize);
        let mut pbuf = [0u8; 256];
        pbuf[..pn].copy_from_slice(
            &wire[PlaintextCodec::HEADER_WIRE_SIZE..PlaintextCodec::HEADER_WIRE_SIZE + pn],
        );
        let frame = codec.decode_frame(header, &mut pbuf[..pn]).unwrap();
        let decoded: SetupConnection<'_> = frame_to_message(frame).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn setup_connection_success_roundtrip() {
        let original = SetupConnectionSuccess {
            used_version: 2,
            flags: 0,
        };
        let mut codec = PlaintextCodec;
        let mut wire = [0u8; 32];
        let _n = codec.encode(&original, &mut wire).unwrap();

        let mut hbuf = [0u8; PlaintextCodec::HEADER_WIRE_SIZE];
        hbuf.copy_from_slice(&wire[..PlaintextCodec::HEADER_WIRE_SIZE]);
        let header = codec.decode_header(&mut hbuf).unwrap();
        let pn = PlaintextCodec::payload_wire_size(header.msg_length as usize);
        let mut pbuf = [0u8; 32];
        pbuf[..pn].copy_from_slice(
            &wire[PlaintextCodec::HEADER_WIRE_SIZE..PlaintextCodec::HEADER_WIRE_SIZE + pn],
        );
        let frame = codec.decode_frame(header, &mut pbuf[..pn]).unwrap();
        let decoded: SetupConnectionSuccess = frame_to_message(frame).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn setup_connection_success_byte_exact() {
        let original = SetupConnectionSuccess {
            used_version: 0x0002,
            flags: 0x0000_0001,
        };
        let mut codec = PlaintextCodec;
        let mut wire = [0u8; 32];
        let n = codec.encode(&original, &mut wire).unwrap();
        // Header: ext=0, msg=1, channel=false, msg_length=6
        //   bytes: [0x00, 0x00, 0x01, 0x06, 0x00, 0x00]
        // Body: u16 used_version=2 LE → [0x02, 0x00], u32 flags=1 LE → [0x01, 0x00, 0x00, 0x00]
        assert_eq!(
            &wire[..n],
            &[
                0x00, 0x00, 0x01, 0x06, 0x00, 0x00, // header
                0x02, 0x00, 0x01, 0x00, 0x00, 0x00, // body
            ]
        );
    }

    #[test]
    fn setup_connection_error_roundtrip() {
        let original = SetupConnectionError {
            flags: 0x42,
            code: Str0_255::try_from("invalid-flags").unwrap(),
        };
        let mut codec = PlaintextCodec;
        let mut wire = [0u8; 64];
        let _n = codec.encode(&original, &mut wire).unwrap();

        let mut hbuf = [0u8; PlaintextCodec::HEADER_WIRE_SIZE];
        hbuf.copy_from_slice(&wire[..PlaintextCodec::HEADER_WIRE_SIZE]);
        let header = codec.decode_header(&mut hbuf).unwrap();
        let pn = PlaintextCodec::payload_wire_size(header.msg_length as usize);
        let mut pbuf = [0u8; 64];
        pbuf[..pn].copy_from_slice(
            &wire[PlaintextCodec::HEADER_WIRE_SIZE..PlaintextCodec::HEADER_WIRE_SIZE + pn],
        );
        let frame = codec.decode_frame(header, &mut pbuf[..pn]).unwrap();
        let decoded: SetupConnectionError<'_> = frame_to_message(frame).unwrap();
        assert_eq!(decoded, original);
    }

    use ii_sv2_types::Bytes0_32;

    #[test]
    fn open_mining_channel_error_roundtrip() {
        let original = OpenMiningChannelError {
            request_id: 42,
            code: Str0_255::try_from(OpenMiningChannelError::UNKNOWN_USER).unwrap(),
        };
        let mut codec = PlaintextCodec;
        let mut wire = [0u8; 64];
        let _n = codec.encode(&original, &mut wire).unwrap();

        let mut hbuf = [0u8; PlaintextCodec::HEADER_WIRE_SIZE];
        hbuf.copy_from_slice(&wire[..PlaintextCodec::HEADER_WIRE_SIZE]);
        let header = codec.decode_header(&mut hbuf).unwrap();
        let pn = PlaintextCodec::payload_wire_size(header.msg_length as usize);
        let mut pbuf = [0u8; 64];
        pbuf[..pn].copy_from_slice(
            &wire[PlaintextCodec::HEADER_WIRE_SIZE..PlaintextCodec::HEADER_WIRE_SIZE + pn],
        );
        let frame = codec.decode_frame(header, &mut pbuf[..pn]).unwrap();
        let decoded: OpenMiningChannelError<'_> = frame_to_message(frame).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn new_mining_job_roundtrip_with_min_ntime() {
        let original = NewMiningJob {
            channel_id: 100,
            job_id: 7,
            min_ntime: Some(1_700_000_000),
            version: 0x2000_0000,
            merkle_root: Uint256::from_le_bytes(&TARGET_PATTERN),
        };
        let mut codec = PlaintextCodec;
        let mut wire = [0u8; 128];
        let _n = codec.encode(&original, &mut wire).unwrap();

        let mut hbuf = [0u8; PlaintextCodec::HEADER_WIRE_SIZE];
        hbuf.copy_from_slice(&wire[..PlaintextCodec::HEADER_WIRE_SIZE]);
        let header = codec.decode_header(&mut hbuf).unwrap();
        assert!(header.is_channel_message);
        let pn = PlaintextCodec::payload_wire_size(header.msg_length as usize);
        let mut pbuf = [0u8; 128];
        pbuf[..pn].copy_from_slice(
            &wire[PlaintextCodec::HEADER_WIRE_SIZE..PlaintextCodec::HEADER_WIRE_SIZE + pn],
        );
        let frame = codec.decode_frame(header, &mut pbuf[..pn]).unwrap();
        let decoded: NewMiningJob<'_> = frame_to_message(frame).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn new_mining_job_roundtrip_future_job() {
        // min_ntime: None signals a future job activated by SetNewPrevHash.
        let original = NewMiningJob {
            channel_id: 100,
            job_id: 7,
            min_ntime: None,
            version: 0x2000_0000,
            merkle_root: Uint256::from_le_bytes(&TARGET_PATTERN),
        };
        let mut codec = PlaintextCodec;
        let mut wire = [0u8; 128];
        let _n = codec.encode(&original, &mut wire).unwrap();

        let mut hbuf = [0u8; PlaintextCodec::HEADER_WIRE_SIZE];
        hbuf.copy_from_slice(&wire[..PlaintextCodec::HEADER_WIRE_SIZE]);
        let header = codec.decode_header(&mut hbuf).unwrap();
        let pn = PlaintextCodec::payload_wire_size(header.msg_length as usize);
        let mut pbuf = [0u8; 128];
        pbuf[..pn].copy_from_slice(
            &wire[PlaintextCodec::HEADER_WIRE_SIZE..PlaintextCodec::HEADER_WIRE_SIZE + pn],
        );
        let frame = codec.decode_frame(header, &mut pbuf[..pn]).unwrap();
        let decoded: NewMiningJob<'_> = frame_to_message(frame).unwrap();
        assert_eq!(decoded, original);
        assert!(decoded.min_ntime.is_none());
    }

    #[test]
    fn open_standard_mining_channel_success_roundtrip() {
        let extranonce_bytes = [0xaa_u8, 0xbb, 0xcc, 0xdd];
        let original = OpenStandardMiningChannelSuccess {
            request_id: 1,
            channel_id: 100,
            target: Uint256::from_le_bytes(&TARGET_PATTERN),
            extranonce_prefix: Bytes0_32::try_from(&extranonce_bytes[..]).unwrap(),
            group_channel_id: 7,
        };
        let mut codec = PlaintextCodec;
        let mut wire = [0u8; 128];
        let _n = codec.encode(&original, &mut wire).unwrap();

        let mut hbuf = [0u8; PlaintextCodec::HEADER_WIRE_SIZE];
        hbuf.copy_from_slice(&wire[..PlaintextCodec::HEADER_WIRE_SIZE]);
        let header = codec.decode_header(&mut hbuf).unwrap();
        let pn = PlaintextCodec::payload_wire_size(header.msg_length as usize);
        let mut pbuf = [0u8; 128];
        pbuf[..pn].copy_from_slice(
            &wire[PlaintextCodec::HEADER_WIRE_SIZE..PlaintextCodec::HEADER_WIRE_SIZE + pn],
        );
        let frame = codec.decode_frame(header, &mut pbuf[..pn]).unwrap();
        let decoded: OpenStandardMiningChannelSuccess<'_> = frame_to_message(frame).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn set_target_roundtrip() {
        let original = SetTarget {
            channel_id: 100,
            max_target: Uint256::from_le_bytes(&TARGET_PATTERN),
        };
        let mut codec = PlaintextCodec;
        let mut wire = [0u8; 64];
        let _n = codec.encode(&original, &mut wire).unwrap();

        let mut hbuf = [0u8; PlaintextCodec::HEADER_WIRE_SIZE];
        hbuf.copy_from_slice(&wire[..PlaintextCodec::HEADER_WIRE_SIZE]);
        let header = codec.decode_header(&mut hbuf).unwrap();
        assert!(header.is_channel_message);
        let pn = PlaintextCodec::payload_wire_size(header.msg_length as usize);
        let mut pbuf = [0u8; 64];
        pbuf[..pn].copy_from_slice(
            &wire[PlaintextCodec::HEADER_WIRE_SIZE..PlaintextCodec::HEADER_WIRE_SIZE + pn],
        );
        let frame = codec.decode_frame(header, &mut pbuf[..pn]).unwrap();
        let decoded: SetTarget<'_> = frame_to_message(frame).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn set_new_prev_hash_roundtrip() {
        let original = SetNewPrevHash {
            channel_id: 100,
            job_id: 7,
            prev_hash: Uint256::from_le_bytes(&TARGET_PATTERN),
            min_ntime: 1_700_000_000,
            nbits: 0x1700_e9bd,
        };
        let mut codec = PlaintextCodec;
        let mut wire = [0u8; 128];
        let _n = codec.encode(&original, &mut wire).unwrap();

        let mut hbuf = [0u8; PlaintextCodec::HEADER_WIRE_SIZE];
        hbuf.copy_from_slice(&wire[..PlaintextCodec::HEADER_WIRE_SIZE]);
        let header = codec.decode_header(&mut hbuf).unwrap();
        assert!(header.is_channel_message);
        let pn = PlaintextCodec::payload_wire_size(header.msg_length as usize);
        let mut pbuf = [0u8; 128];
        pbuf[..pn].copy_from_slice(
            &wire[PlaintextCodec::HEADER_WIRE_SIZE..PlaintextCodec::HEADER_WIRE_SIZE + pn],
        );
        let frame = codec.decode_frame(header, &mut pbuf[..pn]).unwrap();
        let decoded: SetNewPrevHash<'_> = frame_to_message(frame).unwrap();
        assert_eq!(decoded, original);
    }

    use ii_sv2_framing::MessageGroup;

    #[test]
    fn dispatch_setup_connection() {
        let outbound = SetupConnection {
            protocol: 0,
            min_version: 2,
            max_version: 2,
            flags: 0,
            endpoint_host: Str0_255::try_from("p").unwrap(),
            endpoint_port: 1,
            device: DeviceInfo {
                vendor: Str0_255::try_from("v").unwrap(),
                hw_rev: Str0_255::try_from("h").unwrap(),
                fw_ver: Str0_255::try_from("f").unwrap(),
                dev_id: Str0_255::try_from("d").unwrap(),
            },
        };
        let mut codec = PlaintextCodec;
        let mut wire = [0u8; 128];
        let _n = codec.encode(&outbound, &mut wire).unwrap();

        let mut hbuf = [0u8; PlaintextCodec::HEADER_WIRE_SIZE];
        hbuf.copy_from_slice(&wire[..PlaintextCodec::HEADER_WIRE_SIZE]);
        let header = codec.decode_header(&mut hbuf).unwrap();
        let pn = PlaintextCodec::payload_wire_size(header.msg_length as usize);
        let mut pbuf = [0u8; 128];
        pbuf[..pn].copy_from_slice(
            &wire[PlaintextCodec::HEADER_WIRE_SIZE..PlaintextCodec::HEADER_WIRE_SIZE + pn],
        );
        let frame = codec.decode_frame(header, &mut pbuf[..pn]).unwrap();

        match MiningMessages::dispatch(frame).unwrap() {
            MiningMessages::SetupConnection(m) => assert_eq!(m, outbound),
            other => panic!("expected SetupConnection, got {:?}", other),
        }
    }

    #[test]
    fn dispatch_open_standard_mining_channel() {
        let outbound = OpenStandardMiningChannel {
            request_id: 1,
            user: Str0_255::try_from("u").unwrap(),
            nominal_hashrate: 1.0,
            max_target: Uint256::from_le_bytes(&TARGET_PATTERN),
        };
        let mut codec = PlaintextCodec;
        let mut wire = [0u8; 128];
        let _n = codec.encode(&outbound, &mut wire).unwrap();

        let mut hbuf = [0u8; PlaintextCodec::HEADER_WIRE_SIZE];
        hbuf.copy_from_slice(&wire[..PlaintextCodec::HEADER_WIRE_SIZE]);
        let header = codec.decode_header(&mut hbuf).unwrap();
        let pn = PlaintextCodec::payload_wire_size(header.msg_length as usize);
        let mut pbuf = [0u8; 128];
        pbuf[..pn].copy_from_slice(
            &wire[PlaintextCodec::HEADER_WIRE_SIZE..PlaintextCodec::HEADER_WIRE_SIZE + pn],
        );
        let frame = codec.decode_frame(header, &mut pbuf[..pn]).unwrap();

        match MiningMessages::dispatch(frame).unwrap() {
            MiningMessages::OpenStandardMiningChannel(m) => assert_eq!(m, outbound),
            other => panic!("expected OpenStandardMiningChannel, got {:?}", other),
        }
    }

    #[test]
    fn dispatch_new_mining_job() {
        let outbound = NewMiningJob {
            channel_id: 100,
            job_id: 7,
            min_ntime: Some(1),
            version: 0,
            merkle_root: Uint256::from_le_bytes(&TARGET_PATTERN),
        };
        let mut codec = PlaintextCodec;
        let mut wire = [0u8; 128];
        let _n = codec.encode(&outbound, &mut wire).unwrap();

        let mut hbuf = [0u8; PlaintextCodec::HEADER_WIRE_SIZE];
        hbuf.copy_from_slice(&wire[..PlaintextCodec::HEADER_WIRE_SIZE]);
        let header = codec.decode_header(&mut hbuf).unwrap();
        assert!(header.is_channel_message);
        let pn = PlaintextCodec::payload_wire_size(header.msg_length as usize);
        let mut pbuf = [0u8; 128];
        pbuf[..pn].copy_from_slice(
            &wire[PlaintextCodec::HEADER_WIRE_SIZE..PlaintextCodec::HEADER_WIRE_SIZE + pn],
        );
        let frame = codec.decode_frame(header, &mut pbuf[..pn]).unwrap();

        match MiningMessages::dispatch(frame).unwrap() {
            MiningMessages::NewMiningJob(m) => assert_eq!(m, outbound),
            other => panic!("expected NewMiningJob, got {:?}", other),
        }
    }

    #[test]
    fn submit_shares_standard_roundtrip() {
        let original = SubmitSharesStandard {
            channel_id: 100,
            sequence_number: 1,
            job_id: 7,
            nonce: 0xdead_beef,
            ntime: 0x6044_9522,
            version: 0x2000_0010,
        };
        let mut codec = PlaintextCodec;
        let mut wire = [0u8; 64];
        let _n = codec.encode(&original, &mut wire).unwrap();

        let mut hbuf = [0u8; PlaintextCodec::HEADER_WIRE_SIZE];
        hbuf.copy_from_slice(&wire[..PlaintextCodec::HEADER_WIRE_SIZE]);
        let header = codec.decode_header(&mut hbuf).unwrap();
        assert!(header.is_channel_message);
        let pn = PlaintextCodec::payload_wire_size(header.msg_length as usize);
        let mut pbuf = [0u8; 64];
        pbuf[..pn].copy_from_slice(
            &wire[PlaintextCodec::HEADER_WIRE_SIZE..PlaintextCodec::HEADER_WIRE_SIZE + pn],
        );
        let frame = codec.decode_frame(header, &mut pbuf[..pn]).unwrap();
        let decoded: SubmitSharesStandard = frame_to_message(frame).unwrap();
        assert_eq!(decoded, original);

        match MiningMessages::dispatch(frame).unwrap() {
            MiningMessages::SubmitSharesStandard(m) => assert_eq!(m, original),
            other => panic!("expected SubmitSharesStandard, got {:?}", other),
        }
    }

    #[test]
    fn submit_shares_success_roundtrip() {
        let original = SubmitSharesSuccess {
            channel_id: 100,
            last_sequence_number: 42,
            new_submits_accepted_count: 7,
            new_shares_sum: 0x0123_4567_89ab_cdef,
        };
        let mut codec = PlaintextCodec;
        let mut wire = [0u8; 64];
        let _n = codec.encode(&original, &mut wire).unwrap();

        let mut hbuf = [0u8; PlaintextCodec::HEADER_WIRE_SIZE];
        hbuf.copy_from_slice(&wire[..PlaintextCodec::HEADER_WIRE_SIZE]);
        let header = codec.decode_header(&mut hbuf).unwrap();
        assert!(header.is_channel_message);
        let pn = PlaintextCodec::payload_wire_size(header.msg_length as usize);
        let mut pbuf = [0u8; 64];
        pbuf[..pn].copy_from_slice(
            &wire[PlaintextCodec::HEADER_WIRE_SIZE..PlaintextCodec::HEADER_WIRE_SIZE + pn],
        );
        let frame = codec.decode_frame(header, &mut pbuf[..pn]).unwrap();
        let decoded: SubmitSharesSuccess = frame_to_message(frame).unwrap();
        assert_eq!(decoded, original);

        match MiningMessages::dispatch(frame).unwrap() {
            MiningMessages::SubmitSharesSuccess(m) => assert_eq!(m, original),
            other => panic!("expected SubmitSharesSuccess, got {:?}", other),
        }
    }

    #[test]
    fn submit_shares_error_roundtrip() {
        let original = SubmitSharesError {
            channel_id: 100,
            sequence_number: 5,
            error_code: Str0_255::try_from(SubmitSharesError::STALE_SHARE).unwrap(),
        };
        let mut codec = PlaintextCodec;
        let mut wire = [0u8; 128];
        let _n = codec.encode(&original, &mut wire).unwrap();

        let mut hbuf = [0u8; PlaintextCodec::HEADER_WIRE_SIZE];
        hbuf.copy_from_slice(&wire[..PlaintextCodec::HEADER_WIRE_SIZE]);
        let header = codec.decode_header(&mut hbuf).unwrap();
        assert!(header.is_channel_message);
        let pn = PlaintextCodec::payload_wire_size(header.msg_length as usize);
        let mut pbuf = [0u8; 128];
        pbuf[..pn].copy_from_slice(
            &wire[PlaintextCodec::HEADER_WIRE_SIZE..PlaintextCodec::HEADER_WIRE_SIZE + pn],
        );
        let frame = codec.decode_frame(header, &mut pbuf[..pn]).unwrap();
        let decoded: SubmitSharesError<'_> = frame_to_message(frame).unwrap();
        assert_eq!(decoded, original);

        match MiningMessages::dispatch(frame).unwrap() {
            MiningMessages::SubmitSharesError(m) => assert_eq!(m, original),
            other => panic!("expected SubmitSharesError, got {:?}", other),
        }
    }
}
