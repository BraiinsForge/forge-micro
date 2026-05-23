use super::*;
use portable_atomic::Ordering as AtomicOrdering;

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
    let resp = HwSolution {
        nonce: 0x1ABA_825A,
        _extra_diff: 0,
        work_id: 0xB8,
        version_idx: 312,
    };
    assert_eq!(resp.rolled_version(0x2000_0000), 0x2027_0000);
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
    let frame = build_write_register(RawChipAddress::All, 0x58, 0x0001_1111);
    assert_eq!(
        frame.as_slice(),
        [
            0x55, 0xAA, 0x51, 0x09, 0x00, 0x58, 0x00, 0x01, 0x11, 0x11, 0x0D
        ]
    );
}

#[test]
fn build_unicast_write() {
    let frame = build_write_register(RawChipAddress::Chip(3), 0x58, 0x0001_1111);
    assert_eq!(
        frame.as_slice(),
        [
            0x55, 0xAA, 0x41, 0x09, 0x03, 0x58, 0x00, 0x01, 0x11, 0x11, 0x07
        ]
    );
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
    let resp = parser.pop().expect("should parse");
    match resp {
        Response::RegisterResponse(r) => {
            assert_eq!(r.value, 0x1362_0300);
            assert_eq!(r.chip_addr, 0x00);
            assert_eq!(r.reg, 0x00);
        }
        Response::HwSolution(_) => panic!("expected register response"),
    }
}

#[test]
fn parse_with_junk() {
    // `UART_DESYNC_BYTES` is process-global, so record the baseline
    // before feeding two junk bytes and assert the post-feed counter
    // is exactly two higher. The absolute value is not asserted
    // because other tests in the same binary may have already bumped
    // the counter. The delta assertion assumes this is the only test
    // that feeds desync bytes; cargo runs tests concurrently by
    // default, so a future test that also exercises the desync path
    // must either gate on `serial_test` or stop reading this counter.
    let before = crate::bm1362::UART_DESYNC_BYTES.load(AtomicOrdering::SeqCst);
    let mut parser = ResponseParser::new();
    feed(
        &mut parser,
        &[
            0xFF, 0xFF, 0xAA, 0x55, 0x13, 0x62, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x1E,
        ],
    );
    let resp = parser.pop().expect("should parse despite junk");
    assert!(matches!(resp, Response::RegisterResponse(_)));
    let after = crate::bm1362::UART_DESYNC_BYTES.load(AtomicOrdering::SeqCst);
    assert_eq!(
        after.wrapping_sub(before),
        2,
        "expected exactly two desync bytes accounted (got {before} → {after})",
    );
}

/// Locks ubos-miner's local share-verification hash to
/// `mining_protocol::hashing::header_hash`. Both sides reconstruct the
/// same canonical block header from the same `WorkAssignment` fields,
/// and the pool will reject any submit whose chip-side hash diverges
/// from what it recomputes for the same `(prev_hash, ntime, nonce, ...)`.
#[test]
fn compute_hash_matches_mining_protocol_reference() {
    let mut stratum_prev = [0u8; 32];
    for (index, byte) in stratum_prev.iter_mut().enumerate() {
        *byte = index as u8;
    }
    let mut merkle = [0u8; 32];
    for (index, byte) in merkle.iter_mut().enumerate() {
        *byte = (32 - index) as u8;
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
    let resp = HwSolution {
        nonce,
        _extra_diff: 0,
        work_id: 0,
        version_idx: 0,
    };

    let actual_hash = hw_work.compute_hash(&resp);
    let reference =
        mining_protocol::hashing::header_hash(version, stratum_prev, merkle, ntime, nbits, nonce);
    assert_eq!(actual_hash, reference);
}
