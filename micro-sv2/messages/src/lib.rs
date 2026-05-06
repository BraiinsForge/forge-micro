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

#[cfg(test)]
mod tests {
    use super::*;
    use ii_sv2_types::{Decode, Encode, Str0_255};

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
}
