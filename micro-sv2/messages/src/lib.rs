//! Stratum V2 elementary mining messages — borrowed-first, `no_std`.
//!
//! See `docs/superpowers/specs/2026-05-06-messages-design.md`.

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
    pub const REQUIRES_STANDARD_JOBS:   u32 = 1;
    pub const REQUIRES_WORK_SELECTION:  u32 = 1 << 1;
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
    /// Expected hashrate in h/s. Proxies with no devices yet send 0.0.
    pub nominal_hashrate: f32,
    pub max_target: Uint256<'a>,
}

impl<'a> Encode for OpenStandardMiningChannel<'a> {
    fn encoded_len(&self) -> usize {
        self.request_id.encoded_len()
            + self.user.encoded_len()
            + self.nominal_hashrate.encoded_len()
            + self.max_target.encoded_len()
    }

    fn encode_to(&self, out: &mut [u8]) -> Result<usize, TypeError> {
        let mut n = 0;
        n += self.request_id.encode_to(&mut out[n..])?;
        n += self.user.encode_to(&mut out[n..])?;
        n += self.nominal_hashrate.encode_to(&mut out[n..])?;
        n += self.max_target.encode_to(&mut out[n..])?;
        Ok(n)
    }
}

impl<'de> Decode<'de> for OpenStandardMiningChannel<'de> {
    fn decode(input: &mut &'de [u8]) -> Result<Self, TypeError> {
        Ok(Self {
            request_id: u32::decode(input)?,
            user: Str0_255::decode(input)?,
            nominal_hashrate: f32::decode(input)?,
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
    pub const UNKNOWN_USER:          &'static str = "unknown-user";
    pub const TOO_MANY_CHANNELS:     &'static str = "too-many-channels";
    pub const DUPLICATED_REQUEST_ID: &'static str = "duplicate-request_id";
    pub const TOO_LARGE_EXTRANONCE:  &'static str = "too-large-extranonce-requested";
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
    const DESCRIPTOR: MessageDescriptor =
        MessageDescriptor::new(MINING_EXTENSION_TYPE, 0x15, true);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ii_sv2_types::{Decode, Encode, Str0_255};
    use ii_sv2_types::Uint256;

    const TARGET_PATTERN: [u8; 32] = [
        0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07,
        0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f,
        0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17,
        0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e, 0x1f,
    ];

    #[test]
    fn open_standard_mining_channel_roundtrip() {
        let original = OpenStandardMiningChannel {
            request_id: 1,
            user: Str0_255::try_from("user.worker1").unwrap(),
            nominal_hashrate: 1.0e12,
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
                0x00, 0x00, 0x01, 0x06, 0x00, 0x00,                   // header
                0x02, 0x00, 0x01, 0x00, 0x00, 0x00,                   // body
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
}
