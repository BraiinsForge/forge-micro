use super::*;

#[test]
fn control_command_chain_inactive_packs_to_broadcast() {
    // chain_inactive() is the broadcast preamble issued three times at
    // bring-up to drop assigned addresses; the encoder reads code,
    // broadcast, and hw_addr off the value, so accidental field churn
    // surfaces here before it reaches the wire.
    let cmd = ControlCommand::chain_inactive();
    assert_eq!(cmd.code(), ControlCode::ChainInactive);
    assert!(cmd.is_broadcast());
    assert_eq!(cmd.hw_addr(), 0x00);
}

#[test]
fn control_command_set_chip_address_packs_to_unicast() {
    // set_chip_address(addr) is unicast (broadcasting would assign the
    // same address to every chip); the caller walks one per slot.
    let cmd = ControlCommand::set_chip_address(0x02);
    assert_eq!(cmd.code(), ControlCode::SetChipAddress);
    assert!(!cmd.is_broadcast());
    assert_eq!(cmd.hw_addr(), 0x02);
}

#[test]
fn control_code_numeric_values_match_spec() {
    // The cmd byte's low 3 bits hold the control code; the protocol
    // pins SetChipAddress=0 and ChainInactive=3.
    assert_eq!(ControlCode::SetChipAddress.as_u8(), 0);
    assert_eq!(ControlCode::ChainInactive.as_u8(), 3);
}

#[test]
fn crc5_broadcast_write_0x58() {
    let payload = [0x51, 0x09, 0x00, 0x58, 0x00, 0x01, 0x11, 0x11];
    assert_eq!(crc5(&payload, 0x1F, true), 0x0D);
}

#[test]
fn crc5_unicast_write_0x58() {
    let payload = [0x41, 0x09, 0x06, 0x58, 0x00, 0x01, 0x11, 0x11];
    assert_eq!(crc5(&payload, 0x1F, true), 0x01);
}

#[test]
fn crc5_broadcast_write_0x54() {
    let payload = [0x51, 0x09, 0x00, 0x54, 0x00, 0x00, 0x00, 0x03];
    assert_eq!(crc5(&payload, 0x1F, true), 0x1D);
}

#[test]
fn crc5_register_response() {
    let data = [0x13, 0x62, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00];
    assert_eq!(crc5(&data, 0x03, false), 0x1E);
}

#[test]
fn work_response_rolled_version() {
    let response = HwSolution {
        nonce: 0x1ABA_825A,
        _extra_diff: 0,
        work_id: 0xB8,
        version_idx: 312,
    };
    assert_eq!(response.rolled_version(0x2000_0000), 0x2027_0000);
}

#[test]
fn serialize_work_frame_crc() {
    // From the worked example in BM1362_WORK.md
    let expected_bytes: [u8; 88] = [
        0x55, 0xAA, 0x21, 0x36, 0xB8, 0x01, 0x00, 0x00, 0x00, 0x00, 0x7E, 0x3C, 0x02, 0x17, 0x91,
        0x22, 0x93, 0x69, 0xEE, 0xF3, 0xAC, 0x4B, 0x19, 0x9E, 0x64, 0xA0, 0x40, 0x43, 0xFD, 0xD8,
        0x16, 0xBD, 0x00, 0x37, 0x8D, 0xC6, 0xF6, 0x78, 0xD2, 0xFF, 0xCD, 0xBD, 0xEA, 0x83, 0x07,
        0xE2, 0xC8, 0xCA, 0xB9, 0xC2, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x97, 0x30,
        0x01, 0x00, 0xC1, 0x8F, 0x5A, 0x80, 0xB5, 0x97, 0xC1, 0x5D, 0x14, 0x03, 0xC7, 0xEE, 0xD8,
        0x81, 0xDC, 0xBB, 0x65, 0x82, 0x9C, 0x62, 0x00, 0x00, 0x00, 0x20, 0x37, 0xBD,
    ];
    let crc = crc16_ccitt(&expected_bytes[2..86]);
    assert_eq!(crc, 0x37BD);
}

#[test]
fn build_broadcast_write() {
    let frame = build_write_register(ChipAddress::All, 0x58, 0x0001_1111);
    assert_eq!(
        frame.as_slice(),
        [
            0x55, 0xAA, 0x51, 0x09, 0x00, 0x58, 0x00, 0x01, 0x11, 0x11, 0x0D
        ]
    );
}

#[test]
fn build_unicast_write() {
    let frame = build_write_register(ChipAddress::Chip(3), 0x58, 0x0001_1111);
    assert_eq!(
        frame.as_slice(),
        [
            0x55, 0xAA, 0x41, 0x09, 0x03, 0x58, 0x00, 0x01, 0x11, 0x11, 0x07
        ]
    );
}

#[test]
fn build_command_chain_inactive_matches_bm1362_payload() {
    // The BM1362 chain-inactive command is the broadcast vil-ctl op
    // (code 3) encoded as raw bytes [0x53, 0x05, 0x00, 0x00] (cmd,
    // length 5, addr 0, reserved 0); the helper must produce the same
    // payload, CRC included.
    let frame = build_command(ControlCommand::chain_inactive());
    let payload = [0x53u8, 0x05, 0x00, 0x00];
    let expected_crc = crc5(&payload, 0x1F, true);
    let mut expected = [0u8; 7];
    expected[0] = 0x55;
    expected[1] = 0xAA;
    expected[2..6].copy_from_slice(&payload);
    expected[6] = expected_crc;
    assert_eq!(frame.as_slice(), expected);
}

#[test]
fn build_command_set_chip_address_matches_bm1362_payload() {
    // The BM1362 set-chip-address command encodes each address as raw
    // bytes [0x40, 0x05, addr, 0x00]; verify the helper packs the same
    // wire payload for each chip slot.
    for hw_addr in [0x00u8, 0x01, 0x02, 0x03] {
        let frame = build_command(ControlCommand::set_chip_address(hw_addr));
        let payload = [0x40u8, 0x05, hw_addr, 0x00];
        let expected_crc = crc5(&payload, 0x1F, true);
        let mut expected = [0u8; 7];
        expected[0] = 0x55;
        expected[1] = 0xAA;
        expected[2..6].copy_from_slice(&payload);
        expected[6] = expected_crc;
        assert_eq!(
            frame.as_slice(),
            expected,
            "set_chip_address({hw_addr:#x}) mismatch"
        );
    }
}

fn feed(parser: &mut ResponseParser, data: &[u8]) {
    parser.free()[..data.len()].copy_from_slice(data);
    parser.commit(data.len());
}

#[test]
fn parse_register_response() {
    let mut parser = ResponseParser::new();
    feed(
        &mut parser,
        &[
            0xAA, 0x55, 0x13, 0x62, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x1E,
        ],
    );
    let response = parser.pop().expect("should parse");
    match response {
        Response::RegisterResponse(register) => {
            assert_eq!(register.value, 0x1362_0300);
            assert_eq!(register.chip_addr, 0x00);
            assert_eq!(register.reg, 0x00);
        }
        Response::HwSolution(_) => panic!("expected register response"),
    }
}

#[test]
fn parse_with_junk() {
    // Assert the parser's local desync counter increments by exactly two
    // for the two junk bytes preceding a valid frame. The counter is
    // local to the parser, so there is no race with other tests.
    let mut parser = ResponseParser::new();
    feed(
        &mut parser,
        &[
            0xFF, 0xFF, 0xAA, 0x55, 0x13, 0x62, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x1E,
        ],
    );
    let response = parser.pop().expect("should parse despite junk");
    assert!(matches!(response, Response::RegisterResponse(_)));
    let desync = parser.take_desync();
    assert_eq!(
        desync, 2,
        "expected exactly two desync bytes (got {desync})",
    );
}

/// Locks ubos-mining's local share-verification hash to
/// `mining_protocol::hashing::header_hash`. Both sides reconstruct the
/// same canonical block header from the same `WorkAssignment` fields,
/// and the pool will reject any submit whose chip-side hash diverges
/// from what it recomputes for the same `(prev_hash, ntime, nonce, ...)`.
#[test]
fn compute_hash_matches_mining_protocol_reference() {
    let mut stratum_prev = [0u8; 32];
    for (index, byte) in stratum_prev.iter_mut().enumerate() {
        // `enumerate` over a 32-byte array yields 0..32.
        *byte = u8::try_from(index).expect("BUG: index < 32");
    }
    let mut merkle = [0u8; 32];
    for (index, byte) in merkle.iter_mut().enumerate() {
        // `enumerate` over a 32-byte array yields 0..32.
        *byte = u8::try_from(32 - index).expect("BUG: 32 - index in 1..=32");
    }
    let version: u32 = 0x2000_0000;
    let ntime: u32 = 0x6700_0000;
    let nbits: u32 = 0x1d00_ffff;
    let nonce: u32 = 0xdead_beef;

    let hw_work = HwWork {
        work_id: 0,
        nonce_start: 0,
        nbits,
        ntime,
        merkle_root: merkle,
        // The stratum adapter word-swaps prev_hash on arrival, so
        // HwWork.prev_hash is already in Bitcoin internal byte
        // order. Simulate that here so the reference header_hash
        // (which swaps inline) produces the same 80-byte header.
        prev_hash: word_swap_prev_hash(&stratum_prev),
        version,
    };
    let response = HwSolution {
        nonce,
        _extra_diff: 0,
        work_id: 0,
        version_idx: 0,
    };

    let actual_hash = hw_work.compute_hash(&response);
    let reference =
        mining_protocol::hashing::header_hash(version, stratum_prev, merkle, ntime, nbits, nonce);
    assert_eq!(actual_hash, reference);
}
