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

//! Hardware SHA-256 driver for the RP2350 on-chip accelerator.
//!
//! The three register helpers below are free functions because the chip
//! has exactly one accelerator. There is nothing for a handle type to
//! own that a second instance could hold differently, and the block's
//! registers are reached through a fixed address either way.
//!
//! The peripheral processes 512-bit blocks: write 16 little-endian u32
//! words to WDATA, then SUM0..SUM7 hold the running digest. Multi-block
//! messages keep writing without re-asserting START. This crate exposes
//! a polled, blocking driver tuned for Bitcoin-style SHA-256d over an
//! 80-byte block header. The polled overhead is roughly 1 microsecond
//! per double-hash at 150 MHz, which is negligible for share-rate
//! rehash workloads.

#![no_std]

use embassy_rp::pac;
use pac::sha256::vals::DmaSize;
use portable_atomic::{AtomicBool, Ordering};

/// Polled, blocking SHA-256d accelerator for the RP2350.
///
/// Wraps the on-chip SHA-256 peripheral. Secure-boot lock
/// `BOOTROM_LOCK_SHA_256` is skipped here because secure boot is
/// disabled on these boards; a future secure-boot build must add the
/// lock before handing the peripheral to application code.
pub struct HardwareSha256d {
    _private: (),
}

impl HardwareSha256d {
    /// Acquires exclusive ownership of the SHA-256 peripheral. Returns
    /// `None` if the driver has already been taken; the peripheral has
    /// no HAL token in embassy-rp 0.10.0, so uniqueness is enforced
    /// here at runtime.
    ///
    /// The peripheral is out of reset by default on RP2350; no explicit
    /// reset deassertion is needed.
    pub fn take() -> Option<Self> {
        static TAKEN: AtomicBool = AtomicBool::new(false);
        if TAKEN.swap(true, Ordering::Relaxed) {
            None
        } else {
            Some(Self { _private: () })
        }
    }

    /// Computes SHA-256d over an 80-byte Bitcoin block header. The hot
    /// path used by share rehash.
    pub fn double_sha256_block_header(&mut self, header: &[u8; 80]) -> [u8; 32] {
        // First pass: 80 input bytes pad to two 64-byte blocks.
        let mut pass1_buffer = [0u32; 32];
        // Copy 80 bytes as 20 u32 words from the header. Each four-byte
        // chunk is assembled via from_le_bytes so no alignment requirement
        // is placed on the header slice.
        for (slot, chunk) in pass1_buffer[..20].iter_mut().zip(header.chunks_exact(4)) {
            *slot = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        }
        // Padding for an 80-byte message under BSWAP=1: byte stream
        // 0x80 followed by zeros and the big-endian 64-bit bit length of
        // 640 bits, encoded as 0x0000_0000_0000_0280 packed little-endian
        // per u32. Indices 21..=30 stay at the buffer's zero-init value.
        pass1_buffer[20] = 0x0000_0080; // [0x80, 0, 0, 0]
        pass1_buffer[31] = 0x8002_0000; // [0, 0, 0x02, 0x80] little-endian

        let pass1_digest = run_pass(&pass1_buffer);

        // Second pass: 32-byte digest pads to a single 64-byte block.
        // After pass1, SUM_n holds the SHA word in big-endian
        // convention. Feeding it back through the BSWAP=1 input path
        // requires swap_bytes so the hardware reverses it back.
        // Indices 9..=14 stay at the buffer's zero-init value.
        let mut pass2_buffer = [0u32; 16];
        for index in 0..8 {
            pass2_buffer[index] = pass1_digest[index].swap_bytes();
        }
        pass2_buffer[8] = 0x0000_0080; // [0x80, 0, 0, 0]
        pass2_buffer[15] = 0x0001_0000; // length 256 bits = [0, 0, 0x01, 0x00] little-endian

        let pass2_digest = run_pass(&pass2_buffer);

        // Output bytes are the SHA digest in canonical big-endian word order.
        let mut output = [0u8; 32];
        for index in 0..8 {
            output[index * 4..index * 4 + 4].copy_from_slice(&pass2_digest[index].to_be_bytes());
        }
        output
    }
}

/// Initialises the SHA-256 peripheral for a new hash computation.
///
/// Clears any pending `ERR_WDATA_NOT_RDY` error, then writes a fresh
/// CSR asserting START, BSWAP, and DMA_SIZE=32-bit.
fn start_hash() {
    let csr = pac::SHA256.csr().read();
    if csr.err_wdata_not_rdy() {
        // Write-one-to-clear the error flag before resetting state.
        let mut clear = csr;
        clear.set_err_wdata_not_rdy(true);
        pac::SHA256.csr().write_value(clear);
    }
    let mut fresh = pac::sha256::regs::Csr(0);
    fresh.set_start(true);
    fresh.set_bswap(true);
    fresh.set_dma_size(DmaSize::_32BIT);
    pac::SHA256.csr().write_value(fresh);
}

/// Feeds a slice of u32 words into the SHA-256 peripheral and waits
/// for the digest to become valid.
///
/// Each word is written only after `WDATA_RDY` is asserted; the final
/// wait spins on `SUM_VLD` before returning.
fn feed_words(words: &[u32]) -> [u32; 8] {
    for &word in words {
        while !pac::SHA256.csr().read().wdata_rdy() {}
        pac::SHA256.wdata().write_value(word);
    }
    while !pac::SHA256.csr().read().sum_vld() {}
    [
        pac::SHA256.sum0().read(),
        pac::SHA256.sum1().read(),
        pac::SHA256.sum2().read(),
        pac::SHA256.sum3().read(),
        pac::SHA256.sum4().read(),
        pac::SHA256.sum5().read(),
        pac::SHA256.sum6().read(),
        pac::SHA256.sum7().read(),
    ]
}

/// Starts a fresh hash and feeds the supplied u32 words. Length is a
/// whole number of 16-word blocks; the caller chooses how many.
fn run_pass(words: &[u32]) -> [u32; 8] {
    start_hash();
    feed_words(words)
}
