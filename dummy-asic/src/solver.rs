//! Nonce search on `APP_CPU`.
//!
//! Hosts the SHA-256d helper plus the embassy task that sweeps nonces for
//! the current Work item.  The task consumes `CURRENT_WORK` and publishes
//! to `SOLUTIONS` both defined in `main.rs`.
//!
//! SHA-256d uses the ESP32-S3 SHA accelerator.  Software SHA on Xtensa
//! is far slower than the UART can drain, so the hardware peripheral
//! is the actual bound this peer is supposed to demonstrate.
//!
//! Bypasses esp-hal in the per-nonce hot path.  Bytes 0..64 of the
//! 80-byte header are constant per Work and would normally be hashed
//! every iteration.  Hashing them once into a midstate and continuing
//! from there per nonce removes one hardware block from the
//! per-iteration cost.  esp-hal exposes this via `ShaDigest::save`/
//! `restore`, but `restore` writes 128 bytes to `m_mem` and 64 bytes
//! to `h_mem` via volatile peripheral-bus stores per nonce, which
//! costs more CPU time than the hardware block saved.  Going through
//! the SHA peripheral's register block directly cuts that to 8 H
//! writes plus 16 M writes per nonce, which is what makes the
//! optimisation pay off here.
//!
//! Hacks worth flagging for anyone touching this file:
//!   * Register addresses are duplicated from the esp32s3 PAC at
//!     `0x6003_b000`.  Going through `register_block().busy()` etc.
//!     does not reliably boil down to a single volatile store, so
//!     the hot loop touches the registers via raw pointers.  If the
//!     PAC ever remaps the SHA peripheral this file will silently
//!     produce wrong hashes.
//!   * The hot loop never writes the SHA mode register.  It assumes
//!     `Sha::start::<Sha256>()` in `capture_midstate` set `MODE` to
//!     SHA-256 and that nothing else has reset it since, which is
//!     true because the solver is the sole owner of the SHA
//!     peripheral.
//!   * Message and digest words are written in native endianness.
//!     The Xtensa LX7 in the ESP32-S3 is little-endian, and the
//!     HAL's `SocDependentEndianess` resolves to native on non-ESP32
//!     parts, so this matches what the HAL's verifier on the host
//!     expects.  The original ESP32 uses a big-endian Tensilica core
//!     and would need the opposite layout.
//!   * The peripheral's H register state is overwritten on every
//!     iteration: `CONTINUE` for the first SHA pulls H from our
//!     midstate write, `START` for the second SHA resets H to the
//!     SHA-256 IV.  Nothing else gets to touch H between those two
//!     points.

use core::ptr;

use esp_hal::peripherals::SHA;
use esp_hal::sha::{Sha, Sha256};
use nb::block;

use crate::protocol::{Solution, Work};
use crate::{CURRENT_WORK, SOLUTIONS};

/// Mask applied to the nonce to trigger a cooperative yield. Power of
/// two so the test compiles to a single `and`. 4096 keeps worst-case
/// new-Work latency well under a millisecond at the hardware's hash
/// rate while amortising the yield/`try_changed` overhead.
const SOLVER_YIELD_MASK: u32 = 4096 - 1;

/// SHA peripheral register addresses.  The PAC defines `peripherals::SHA`
/// at `0x6003_b000`; we duplicate the offsets here so the inner loop can
/// touch the registers without going through the `RegisterBlock`
/// accessors, which the compiler does not always boil down to a single
/// volatile store.
const SHA_BASE: usize = 0x6003_b000;
const SHA_REG_START: *mut u32 = (SHA_BASE + 0x10) as *mut u32;
const SHA_REG_CONTINUE: *mut u32 = (SHA_BASE + 0x14) as *mut u32;
const SHA_REG_BUSY: *const u32 = (SHA_BASE + 0x18) as *const u32;
const SHA_H_MEM: *mut u32 = (SHA_BASE + 0x40) as *mut u32;
const SHA_M_MEM: *mut u32 = (SHA_BASE + 0x80) as *mut u32;

/// Pre-computed padding for the second message block of an 80-byte
/// SHA-256: byte 16 is the `0x80` marker, bytes 17..56 are zero, the
/// last 8 bytes are the BE bit length 640 = `0x280`.  Words are stored
/// in the byte order the SHA peripheral expects when receiving native
/// `u32` writes (matches the layout produced by the HAL's
/// `aligned_volatile_copy`).  Word 4 carries `0x80` in the LSB; word 15
/// is the LE u32 of bytes `[0, 0, 0x02, 0x80]`.
const FIRST_BLOCK_PAD_W4: u32 = 0x0000_0080;
const FIRST_BLOCK_LEN_LO: u32 = 0x8002_0000;

/// Pre-computed padding for the only block of a 32-byte SHA-256: byte
/// 32 is `0x80`, last 8 bytes are the BE bit length 256 = `0x100`.
/// Word 8 carries `0x80`; word 15 is `[0, 0, 0x01, 0x00]` as LE u32.
const SECOND_BLOCK_PAD: [u32; 8] = [0x0000_0080, 0, 0, 0, 0, 0, 0, 0x0001_0000];

/// Return `true` when `hash` (256-bit little-endian integer) is `<=`
/// `target`. Mirrors `cpu_miner::hash_meets_target`; duplicated here
/// because the dummy-asic firmware is xtensa-only and `cpu-miner` is
/// unavailable: pulling it in would drag in an embassy-executor
/// version that conflicts with esp-rtos's pin.
fn hash_meets_target(hash: &[u8; 32], target: &[u8; 32]) -> bool {
    for index in (0..32).rev() {
        if hash[index] < target[index] {
            return true;
        }
        if hash[index] > target[index] {
            return false;
        }
    }
    true
}

/// Read the host-supplied starting nonce from header bytes 76..80. The
/// host advances this after each accepted share so the next sweep
/// continues past the last match instead of re-finding the same low
/// nonces and spamming the pool with duplicates.
fn nonce_start_from_header(header: &[u8; 80]) -> u32 {
    u32::from_le_bytes([header[76], header[77], header[78], header[79]])
}

/// Sweep nonces on the `APP_CPU`.  Yields to the embassy executor each
/// time the nonce hits a `SOLVER_YIELD_MASK` boundary to keep other
/// tasks responsive.  Restarts the sweep whenever a new Work item
/// arrives on `CURRENT_WORK`.
#[embassy_executor::task]
pub(crate) async fn solver_task(sha_peripheral: SHA<'static>) -> ! {
    let mut sha = Sha::new(sha_peripheral);
    // Solver is the sole receiver; reader_task is the only publisher.
    let mut receiver = CURRENT_WORK
        .receiver()
        .expect("BUG: Watch<_, _, 1> already has a receiver");

    let mut work: Work = receiver.changed().await;
    let mut midstate = capture_midstate(&mut sha, &work.header);
    let mut block1 = build_first_message_block(&work.header);
    let mut nonce = nonce_start_from_header(&work.header);

    loop {
        // Header bytes 76..80 carry the nonce in little-endian, so the
        // u32 message word that covers those bytes is the nonce itself
        // when written as a native `u32` to the SHA peripheral.
        block1[3] = nonce;

        let first_hash_words = raw_first_sha(&midstate, &block1);
        let result_words = raw_second_sha(&first_hash_words);
        let result_hash = words_to_bytes(&result_words);

        if hash_meets_target(&result_hash, &work.target) {
            // try_send drops the new solution on a full channel rather
            // than blocking the solver. At the host's wire-level target
            // the solver produces ~780 shares/s while the writer task
            // drains UART at ~2k frames/s, so the channel stays empty in
            // steady state.
            let _ = SOLUTIONS.try_send(Solution {
                nonce,
                work_id: work.work_id,
                hash: result_hash,
            });
        }

        // Wrapping add is intentional. The dummy ASIC contract assumes the
        // host only sends satisfiable targets, so 2^32 is never reached.
        nonce = nonce.wrapping_add(1);

        if nonce & SOLVER_YIELD_MASK == 0 {
            if let Some(new_work) = receiver.try_changed() {
                work = new_work;
                midstate = capture_midstate(&mut sha, &work.header);
                block1 = build_first_message_block(&work.header);
                nonce = nonce_start_from_header(&work.header);
            }
            embassy_futures::yield_now().await;
        }
    }
}

/// Hash bytes 0..64 of the header through the HAL once, then read the
/// resulting H state straight out of the peripheral.  The HAL `start`
/// also leaves the peripheral in SHA-256 mode for subsequent raw
/// access, which `solver_task` relies on.
fn capture_midstate(sha: &mut Sha<'static>, header: &[u8; 80]) -> [u32; 8] {
    {
        let mut digest = sha.start::<Sha256>();
        let mut remaining = &header[..64];
        while !remaining.is_empty() {
            remaining = block!(digest.update(remaining)).expect("BUG: SHA update is Infallible");
        }
    }
    // Wait for the hardware to finish processing block 1 before
    // sampling the H registers.
    raw_wait_busy();
    let mut midstate = [0u32; 8];
    for (index, word) in midstate.iter_mut().enumerate() {
        *word = unsafe { ptr::read_volatile(SHA_H_MEM.add(index)) };
    }
    midstate
}

/// Pack the constant portion of the second 64-byte block of the first
/// SHA: header bytes 64..76 in words 0..3, the `0x80` padding marker
/// in word 4, zeros in words 5..15, and the low half of the BE bit
/// length in word 15.  Word 14 carries the high half of the length,
/// which is zero for an 80-byte input.  Word 3 (the nonce) is filled
/// per iteration.
fn build_first_message_block(header: &[u8; 80]) -> [u32; 16] {
    let mut block = [0u32; 16];
    block[0] = u32::from_le_bytes([header[64], header[65], header[66], header[67]]);
    block[1] = u32::from_le_bytes([header[68], header[69], header[70], header[71]]);
    block[2] = u32::from_le_bytes([header[72], header[73], header[74], header[75]]);
    // block[3] = nonce, filled by the solver loop.
    block[4] = FIRST_BLOCK_PAD_W4;
    // block[5..14] stay zero.
    block[15] = FIRST_BLOCK_LEN_LO;
    block
}

/// Run the first SHA-256 of the round on the SHA peripheral, continuing
/// from `midstate`.  Writes 8 H words plus 16 M words per call.
fn raw_first_sha(midstate: &[u32; 8], block: &[u32; 16]) -> [u32; 8] {
    unsafe {
        for (index, &word) in midstate.iter().enumerate() {
            ptr::write_volatile(SHA_H_MEM.add(index), word);
        }
        for (index, &word) in block.iter().enumerate() {
            ptr::write_volatile(SHA_M_MEM.add(index), word);
        }
        ptr::write_volatile(SHA_REG_CONTINUE, 1);
    }
    raw_wait_busy();
    let mut output = [0u32; 8];
    for (index, word) in output.iter_mut().enumerate() {
        *word = unsafe { ptr::read_volatile(SHA_H_MEM.add(index)) };
    }
    output
}

/// Run the second SHA-256 over the 32-byte first hash plus pre-computed
/// padding.  `START` resets the peripheral's H register to the SHA-256
/// IV before processing, so no H write is needed here.
fn raw_second_sha(first_hash: &[u32; 8]) -> [u32; 8] {
    unsafe {
        for (index, &word) in first_hash.iter().enumerate() {
            ptr::write_volatile(SHA_M_MEM.add(index), word);
        }
        for (index, &word) in SECOND_BLOCK_PAD.iter().enumerate() {
            ptr::write_volatile(SHA_M_MEM.add(8 + index), word);
        }
        ptr::write_volatile(SHA_REG_START, 1);
    }
    raw_wait_busy();
    let mut output = [0u32; 8];
    for (index, word) in output.iter_mut().enumerate() {
        *word = unsafe { ptr::read_volatile(SHA_H_MEM.add(index)) };
    }
    output
}

fn raw_wait_busy() {
    while unsafe { ptr::read_volatile(SHA_REG_BUSY) } != 0 {}
}

/// The HAL converts H words to bytes via `to_ne_bytes`, which on
/// Xtensa LX7 is `to_le_bytes`.  Mirror that so the resulting
/// `[u8; 32]` matches what the host's verifier expects.
fn words_to_bytes(words: &[u32; 8]) -> [u8; 32] {
    let mut bytes = [0u8; 32];
    for (index, &word) in words.iter().enumerate() {
        bytes[index * 4..index * 4 + 4].copy_from_slice(&word.to_le_bytes());
    }
    bytes
}
