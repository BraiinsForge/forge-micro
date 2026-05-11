use stratum_client::WorkAssignment;

use super::wire::{decode_share, encode_work, frame_start, SHARE_FRAME_LEN, WORK_FRAME_LEN};

fn sample_assignment() -> WorkAssignment {
    WorkAssignment {
        work_id: 0x0102_0304,
        prev_hash: [0x11; 32],
        merkle_root: [0x22; 32],
        target: [0x33; 32],
        version: 0x2000_0000,
        version_mask: 0,
        nbits: 0x1700_e9bd,
        ntime: 0x6A00_1122,
    }
}

#[test]
fn encode_work_frame_layout() {
    let work = sample_assignment();
    let mut frame = [0u8; WORK_FRAME_LEN];
    encode_work(&work, &mut frame);

    assert_eq!(frame[0], frame_start());
    assert_eq!(frame[1], 0x01); // OPCODE_WORK
                                // Header bytes 0..4 are version LE.
    assert_eq!(&frame[2..6], &work.version.to_le_bytes());
    // Prev hash and merkle_root are direct memcpy.
    assert_eq!(&frame[6..38], &work.prev_hash);
    assert_eq!(&frame[38..70], &work.merkle_root);
    // ntime + nbits in LE, nonce field zeroed.
    assert_eq!(&frame[70..74], &work.ntime.to_le_bytes());
    assert_eq!(&frame[74..78], &work.nbits.to_le_bytes());
    assert_eq!(&frame[78..82], &[0, 0, 0, 0]);
    // Target memcpy.
    assert_eq!(&frame[82..114], &work.target);
    // work_id BE.
    assert_eq!(&frame[114..118], &work.work_id.to_be_bytes());
    // XOR-8 checksum over bytes 1..(WORK_FRAME_LEN - 1).
    let expected = frame[1..WORK_FRAME_LEN - 1]
        .iter()
        .fold(0u8, |acc, b| acc ^ b);
    assert_eq!(frame[WORK_FRAME_LEN - 1], expected);
}

#[test]
fn decode_share_round_trip() {
    let mut frame = [0u8; SHARE_FRAME_LEN];
    frame[0] = frame_start();
    frame[1] = 0x02; // OPCODE_SHARE
    frame[2..6].copy_from_slice(&0xdead_beef_u32.to_be_bytes());
    frame[6..10].copy_from_slice(&0x0102_0304_u32.to_be_bytes());
    // Bytes 10..(SHARE_FRAME_LEN - 1) carry per-share extras the
    // host ignores today; leave zero. Checksum over bytes
    // 1..(SHARE_FRAME_LEN - 1).
    let checksum = frame[1..SHARE_FRAME_LEN - 1]
        .iter()
        .fold(0u8, |acc, b| acc ^ b);
    frame[SHARE_FRAME_LEN - 1] = checksum;

    let share = decode_share(&frame).expect("valid share");
    assert_eq!(share.nonce, 0xdead_beef);
    assert_eq!(share.work_id, 0x0102_0304);
}

#[test]
fn decode_share_rejects_wrong_opcode() {
    let mut frame = [0u8; SHARE_FRAME_LEN];
    frame[0] = frame_start();
    frame[1] = 0x99; // not OPCODE_SHARE
    let err = decode_share(&frame).expect_err("opcode mismatch");
    assert!(matches!(err, super::wire::DecodeError::OpcodeMismatch));
}

#[test]
fn decode_share_rejects_bad_checksum() {
    let mut frame = [0u8; SHARE_FRAME_LEN];
    frame[0] = frame_start();
    frame[1] = 0x02; // OPCODE_SHARE
    frame[SHARE_FRAME_LEN - 1] = 0xFF; // wrong checksum
    let err = decode_share(&frame).expect_err("checksum");
    assert!(matches!(err, super::wire::DecodeError::Checksum));
}
