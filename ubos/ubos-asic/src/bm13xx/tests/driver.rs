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

use crate::bm13xx::chip::Chip;
use crate::bm13xx::framing::HwWork;
use crate::bm13xx::framing::RESPONSE_LEN;
use crate::bm13xx::framing::crc5;
use crate::chain::MAX_SOLUTIONS_PER_BATCH;
use crate::test_support::{MockTransport, block_on};
use crate::{ChainDriver, WorkJob};

use super::Bm13xxDriver;

/// A single BM1362 chip-id register reply, the same frame the framing
/// parser test pins: value `0x1362_0300` (chip-id field `0x1362`), reg
/// `0x00`, CRC5 `0x1E`.
const BM1362_CHIP_ID_REPLY: [u8; 11] = [
    0xAA, 0x55, 0x13, 0x62, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x1E,
];

#[test]
fn reset_and_enumerate_counts_matching_responders() {
    let mut driver = Bm13xxDriver::new(Chip::Bm1362);
    let mut io = MockTransport::with_reads(&BM1362_CHIP_ID_REPLY);
    let board = crate::BMM101_HASHBOARD;

    let count = block_on(driver.reset_and_enumerate(&mut io, &board));

    // Exactly one responder reported the BM1362 chip-id.
    assert_eq!(count, 1);
    // The chain was addressed and read: the reset broadcasts, the
    // per-slot address assignment, and the chip-id read all went out.
    assert!(!io.writes.is_empty());
}

#[test]
fn reset_and_enumerate_counts_line_garble_as_desync() {
    let mut driver = Bm13xxDriver::new(Chip::Bm1362);
    // Garble ahead of the reply forces the bring-up read's ephemeral
    // parser to resync; the dropped bytes must reach the driver's
    // desync tally rather than dying with the parser.
    let mut reads: heapless::Vec<u8, 16> = heapless::Vec::new();
    reads
        .extend_from_slice(&[0x00, 0xFF, 0xAA, 0x11])
        .expect("test frames fit");
    reads
        .extend_from_slice(&BM1362_CHIP_ID_REPLY)
        .expect("test frames fit");
    let mut io = MockTransport::with_reads(&reads);
    let board = crate::BMM101_HASHBOARD;

    let count = block_on(driver.reset_and_enumerate(&mut io, &board));

    assert_eq!(count, 1);
    assert_eq!(driver.take_desync(), 4);
}

#[test]
fn reset_and_enumerate_stops_at_the_read_reply_budget_on_a_wedged_line() {
    let mut driver = Bm13xxDriver::new(Chip::Bm1362);
    // A line that never carries the `0xAA 0x55` preamble keeps the
    // ephemeral parser resyncing byte by byte forever; twice the budget
    // so the read stopping is pinned on the budget rather than on the
    // mock line running dry.
    let garbage = [0xFFu8; super::READ_REPLY_BUDGET * 2];
    let mut io = MockTransport::with_reads(&garbage);
    let board = crate::BMM101_HASHBOARD;

    let count = block_on(driver.reset_and_enumerate(&mut io, &board));

    // No chip-id reply was ever parsed off the wedged line.
    assert_eq!(count, 0);
    // The budget cuts the read off at exactly its own byte count rather
    // than draining the whole line.
    assert_eq!(
        driver.take_desync(),
        u32::try_from(super::READ_REPLY_BUDGET).expect("BUG: budget fits u32")
    );
}

#[test]
fn configure_fails_when_the_host_baud_switch_is_refused() {
    let mut driver = Bm13xxDriver::new(Chip::Bm1362);
    let mut io = MockTransport::new();
    io.fail_baud = true;
    let board = crate::BMM101_HASHBOARD;

    let result = block_on(driver.configure(&mut io, &board));

    // The chips have already been told to move, so a refused host
    // switch must fail bring-up rather than return Ok on a misclocked
    // line.
    assert!(matches!(result, Err(crate::ChipError::BaudSwitch(_))));
    // The mock line never moved.
    assert_eq!(io.baud, 0);
}

#[test]
fn send_work_serializes_the_hw_work_frame() {
    let mut driver = Bm13xxDriver::new(Chip::Bm1362);
    // Matches the job's zero version_mask below, so send_work writes
    // only the work frame and no `MidstateCalReg` broadcast.
    driver.programmed_mask = Some(0x0000);
    let mut io = MockTransport::new();
    let job = WorkJob {
        work_id: 2,
        nonce_start: 0,
        nbits: 0x1703_1abc,
        ntime: 0x6512_3456,
        version: 0x2000_0000,
        version_mask: 0,
        merkle_root: [0x11; 32],
        prev_hash: [0x22; 32],
    };

    block_on(driver.send_work(&mut io, &job)).expect("send_work is infallible for a valid job");

    // work_id 2 strided by the BM1362 midstate count (8) is work-id 16.
    let expected = HwWork {
        work_id: 16,
        nonce_start: 0,
        nbits: 0x1703_1abc,
        ntime: 0x6512_3456,
        merkle_root: [0x11; 32],
        prev_hash: [0x22; 32],
        version: 0x2000_0000,
    }
    .serialize();
    assert_eq!(io.writes.as_slice(), expected.as_slice());
}

/// Build the 11-byte work-response frame the chip pushes for a found
/// nonce: `AA 55`, nonce (LE), an opaque byte, the work-id, the
/// version-rolling index (BE), then the work-response CRC5 with the
/// high bit set.
fn work_response(nonce: u32, work_id: u8, version_idx: u16) -> [u8; 11] {
    let mut frame = [0u8; 11];
    frame[0] = 0xAA;
    frame[1] = 0x55;
    frame[2..6].copy_from_slice(&nonce.to_le_bytes());
    frame[6] = 0x00;
    frame[7] = work_id;
    frame[8..10].copy_from_slice(&version_idx.to_be_bytes());
    // Work responses use CRC5 init 0x06 with the high bit flagging a
    // work frame.
    frame[10] = 0x80 | (crc5(&frame[2..10], 0x06, false) & 0x1F);
    frame
}

#[test]
fn ingest_decodes_a_solution_and_recovers_the_work_id() {
    let mut driver = Bm13xxDriver::new(Chip::Bm1362);
    // work-id 16 divided by the BM1362 midstate stride (8) is work_id 2.
    let frame = work_response(0x1234_5678, 16, 0);

    let batch = driver.ingest(&frame);

    assert_eq!(batch.len(), 1);
    assert_eq!(batch[0].work_id, 2);
    assert_eq!(batch[0].nonce, 0x1234_5678);
    assert_eq!(batch[0].ntime_offset, 0);
}

#[test]
fn ingest_reassembles_a_frame_split_across_reads() {
    let mut driver = Bm13xxDriver::new(Chip::Bm1362);
    let frame = work_response(0xdead_beef, 24, 0);

    // No solution until the frame is complete.
    assert!(driver.ingest(&frame[..5]).is_empty());
    let batch = driver.ingest(&frame[5..]);

    assert_eq!(batch.len(), 1);
    // work-id 24 / midstate 8 = work_id 3.
    assert_eq!(batch[0].work_id, 3);
    assert_eq!(batch[0].nonce, 0xdead_beef);
}

#[test]
fn ingest_drops_the_batch_overflow_solution_in_a_single_oversized_call() {
    let mut driver = Bm13xxDriver::new(Chip::Bm1362);
    // `ingest` takes an unbounded byte slice, so nothing stops a single
    // call from carrying more valid, CRC-passing frames than one batch
    // holds. One more than the batch capacity forces the overflow path;
    // the buffer is sized from the same constants so a batch resize
    // cannot silently shrink the test below the overflow it exercises.
    let mut frames: heapless::Vec<u8, { (MAX_SOLUTIONS_PER_BATCH + 1) * 11 }> =
        heapless::Vec::new();
    let last_work_id = u8::try_from(MAX_SOLUTIONS_PER_BATCH).expect("BUG: batch cap fits u8");
    for work_id in 0..=last_work_id {
        frames
            .extend_from_slice(&work_response(
                0x1000_0000 + u32::from(work_id),
                // The stride-8 wire id wraps once the batch outgrows 32
                // frames; the overflow path cares about the count, not
                // id uniqueness.
                work_id.wrapping_mul(8),
                0,
            ))
            .expect("test frames fit");
    }

    let batch = driver.ingest(&frames);

    // The batch caps at MAX_SOLUTIONS_PER_BATCH; the last, already
    // CRC-validated solution is lost rather than returned.
    assert_eq!(batch.len(), MAX_SOLUTIONS_PER_BATCH);
    // By the time the batch overflows the whole slice has already been
    // fed to the parser, so the dropped solution's frame length is the
    // entire desync contribution, with no unfed remainder added to it.
    assert_eq!(
        driver.take_desync(),
        u32::try_from(RESPONSE_LEN).expect("BUG: frame length fits u32")
    );

    // The parser carries no stale partial frame out of the overflow, so
    // the next call still decodes cleanly.
    let frame = work_response(0xdead_beef, 16, 0);
    let next = driver.ingest(&frame);
    assert_eq!(next.len(), 1);
    assert_eq!(next[0].nonce, 0xdead_beef);
}

#[test]
fn bm1370_work_id_packing_round_trips_through_the_driver() {
    let mut driver = Bm13xxDriver::new(Chip::Bm1370);
    // Matches the job's zero version_mask below, so send_work writes
    // only the work frame and no `MidstateCalReg` broadcast.
    driver.programmed_mask = Some(0x0000);
    let mut io = MockTransport::new();
    let job = WorkJob {
        work_id: 3,
        nonce_start: 0,
        nbits: 0x1703_1abc,
        ntime: 0x6512_3456,
        version: 0x2000_0000,
        version_mask: 0,
        merkle_root: [0x11; 32],
        prev_hash: [0x22; 32],
    };

    block_on(driver.send_work(&mut io, &job)).expect("send_work is infallible for a valid job");

    // work_id 3 strided by the BM1370 midstate count (16) is work-id 48.
    let expected = HwWork {
        work_id: 48,
        nonce_start: 0,
        nbits: 0x1703_1abc,
        ntime: 0x6512_3456,
        merkle_root: [0x11; 32],
        prev_hash: [0x22; 32],
        version: 0x2000_0000,
    }
    .serialize();
    assert_eq!(io.writes.as_slice(), expected.as_slice());

    // The BM1370 response packs the id into the high nibble as
    // `(work_id << 1) | small_core_id`; work-id 48 from small core 5
    // arrives as 0x65 and must decode back to work_id 3.
    let frame = work_response(0xdead_beef, 0x65, 0);
    let batch = driver.ingest(&frame);

    assert_eq!(batch.len(), 1);
    assert_eq!(batch[0].work_id, 3);
    assert_eq!(batch[0].nonce, 0xdead_beef);
}

#[test]
fn line_error_drops_the_partial_frame_into_the_desync_tally() {
    let mut driver = Bm13xxDriver::new(Chip::Bm1362);
    let frame = work_response(0x1234_5678, 16, 0);

    // Buffer a partial frame, then fault the line.
    assert!(driver.ingest(&frame[..5]).is_empty());
    driver.line_error();

    // The buffered bytes are dropped and folded into the tally rather
    // than dying with the parser.
    assert_eq!(driver.take_desync(), 5);
    // The pre-fault remainder cannot splice into a solution.
    assert!(driver.ingest(&frame[5..]).is_empty());
}

#[test]
fn reset_clears_desync_and_parser_state_but_keeps_chip_identity() {
    let mut driver = Bm13xxDriver::new(Chip::Bm1370);
    let frame = work_response(0x1234_5678, 0x40, 0);
    let garble: [u8; 4] = [0x00, 0xFF, 0xAA, 0x11];

    // Confirm the garble alone bumps the desync tally. Only 3 of the 4
    // bytes count yet: the parser's preamble scan stops once fewer than
    // two bytes remain, so the trailing byte sits buffered, uncounted,
    // until a later push drops it too.
    assert!(driver.ingest(&garble).is_empty());
    assert_eq!(driver.take_desync(), 3);

    // Bump the tally again and leave a partial frame buffered mid-parse.
    assert!(driver.ingest(&garble).is_empty());
    assert!(driver.ingest(&frame[..5]).is_empty());

    driver.reset();

    // The tally clears with the reset rather than carrying the
    // pre-reset garble over.
    assert_eq!(driver.take_desync(), 0);
    // Chip identity survives the reset: the BM1370 core count still
    // reports off the same descriptor.
    assert_eq!(driver.descriptor().core_count, 2040);
    // The parser state is gone too: the pre-reset partial frame's
    // remaining bytes cannot splice with anything post-reset into a
    // stale solution.
    assert!(driver.ingest(&frame[5..]).is_empty());
}

#[test]
fn capability_metadata_matches_the_chip_model() {
    let bm1362 = Bm13xxDriver::new(Chip::Bm1362);
    let bm1370 = Bm13xxDriver::new(Chip::Bm1370);

    assert_eq!(bm1362.boot_uart_rate(), 115_200);
    // BM1362: 256 work-ids / midstate 8 = 32 slots.
    assert_eq!(bm1362.work_slots(), 32);
    // BM1370: 128 usable work-ids / midstate 16 = 8 slots.
    assert_eq!(bm1370.work_slots(), 8);
    // The chain seam serves the neutral half of the Bitmain descriptor;
    // core counts differ per model, so they pin the driver-to-descriptor
    // wiring.
    assert_eq!(bm1362.descriptor().core_count, 514);
    assert_eq!(bm1370.descriptor().core_count, 2040);
    assert!(matches!(bm1362.result_mode(), crate::ResultMode::Push));
}

#[test]
fn poll_results_is_empty_for_the_push_family() {
    let mut driver = Bm13xxDriver::new(Chip::Bm1362);
    let mut io = MockTransport::new();
    assert!(block_on(driver.poll_results(&mut io)).solutions.is_empty());
}

/// The 11-byte `MidstateCalReg` (`0xa4`) broadcast write `send_work`
/// makes when the job narrows the version-rolling mask, so the tests
/// below assert against exactly what the driver put on the wire.
fn expected_midstate_cal_write(bitmask: u16) -> heapless::Vec<u8, 4096> {
    use crate::bm13xx::registers::Register as _;
    let value = u32::from_be_bytes(
        crate::bm13xx::registers::MidstateCalReg::factory_default()
            .with_version_rolled_bitmask(bitmask)
            .to_be_bytes(),
    );
    let mut out: heapless::Vec<u8, 4096> = heapless::Vec::new();
    out.extend_from_slice(
        crate::bm13xx::framing::build_write_register(
            ubos_core::index::ChipAddress::All,
            0xa4,
            value,
        )
        .as_slice(),
    )
    .expect("test frame fits");
    out
}

/// A job whose `version_mask` narrows the BIP320 field to `0x0000_E000`
/// broadcasts a `MidstateCalReg` write with bitmask `0x0007` (the field
/// shifted down by 13) before the work frame.
#[test]
fn send_work_programs_the_narrowed_version_rolled_bitmask() {
    let mut driver = Bm13xxDriver::new(Chip::Bm1362);
    driver.programmed_mask = Some(0xFFFF);
    let mut io = MockTransport::new();
    let job = WorkJob {
        work_id: 0,
        nonce_start: 0,
        nbits: 0x1703_1abc,
        ntime: 0x6512_3456,
        version: 0x2000_0000,
        version_mask: 0x0000_E000,
        merkle_root: [0x11; 32],
        prev_hash: [0x22; 32],
    };

    block_on(driver.send_work(&mut io, &job)).expect("send_work is infallible for a valid job");

    let expected_register_write = expected_midstate_cal_write(0x0007);
    assert!(io.writes.starts_with(expected_register_write.as_slice()));
}

/// A second `send_work` with the same narrowed mask writes no further
/// `MidstateCalReg` register — the driver remembers what it already
/// programmed.
#[test]
fn send_work_skips_the_register_write_when_the_mask_is_unchanged() {
    let mut driver = Bm13xxDriver::new(Chip::Bm1362);
    driver.programmed_mask = Some(0xFFFF);
    let mut io = MockTransport::new();
    let job = WorkJob {
        work_id: 0,
        nonce_start: 0,
        nbits: 0x1703_1abc,
        ntime: 0x6512_3456,
        version: 0x2000_0000,
        version_mask: 0x0000_E000,
        merkle_root: [0x11; 32],
        prev_hash: [0x22; 32],
    };

    block_on(driver.send_work(&mut io, &job)).expect("send_work is infallible for a valid job");
    let after_first = io.writes.len();
    block_on(driver.send_work(&mut io, &job)).expect("send_work is infallible for a valid job");

    // The second call appended only the work frame: no 0xa4 register
    // write bytes were added.
    let hw_work = HwWork {
        work_id: 0,
        nonce_start: job.nonce_start,
        nbits: job.nbits,
        ntime: job.ntime,
        merkle_root: job.merkle_root,
        prev_hash: job.prev_hash,
        version: job.version,
    }
    .serialize();
    assert_eq!(io.writes.len(), after_first + hw_work.len());
    assert_eq!(&io.writes[after_first..], hw_work.as_slice());
}

/// A job that grants the full BIP320 field after `reset_and_enumerate`
/// (which already broadcasts `MidstateCalReg::factory_default`, whose
/// bitmask is the full `0xFFFF`) causes no register write at all.
#[test]
fn send_work_with_the_full_mask_after_reset_writes_no_register() {
    let mut driver = Bm13xxDriver::new(Chip::Bm1362);
    let mut io = MockTransport::with_reads(&BM1362_CHIP_ID_REPLY);
    let board = crate::BMM101_HASHBOARD;
    block_on(driver.reset_and_enumerate(&mut io, &board));
    io.writes.clear();

    let job = WorkJob {
        work_id: 0,
        nonce_start: 0,
        nbits: 0x1703_1abc,
        ntime: 0x6512_3456,
        version: 0x2000_0000,
        version_mask: crate::chain::BIP320_VERSION_MASK,
        merkle_root: [0x11; 32],
        prev_hash: [0x22; 32],
    };

    block_on(driver.send_work(&mut io, &job)).expect("send_work is infallible for a valid job");

    let hw_work = HwWork {
        work_id: 0,
        nonce_start: job.nonce_start,
        nbits: job.nbits,
        ntime: job.ntime,
        merkle_root: job.merkle_root,
        prev_hash: job.prev_hash,
        version: job.version,
    }
    .serialize();
    assert_eq!(io.writes.as_slice(), hw_work.as_slice());
}
