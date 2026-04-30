//! Bitcoin header math and CPU mining loop.
//!
//! Provides block-header construction, target comparison, double-SHA256
//! hashing, an async nonce-scan loop, and the in-process implementation
//! of `asic_interface::AsicInterface`. `ChannelAsic` and `cpu_miner_task`
//! together pair with a generic worker so the same code path drives the
//! CPU miner here or a real ASIC over UART elsewhere.

#![no_std]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::missing_panics_doc)]
#![allow(clippy::module_name_repetitions)]

#[cfg(test)]
#[path = "tests/header.rs"]
mod tests;

use core::convert::Infallible;

use asic_interface::{AsicInterface, RawShare, WorkFrame};
use embassy_futures::select::{select, Either};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::watch::Watch;
use portable_atomic::Ordering;
use sha2::{Digest, Sha256};

// Yield frequency matters: long stretches between yields starve other
// tasks on the same executor. On rp2040 and stm32 boards that executor
// also runs the embassy-net stack, so slow yields surface as
// SocketWrite errors and missed DNS/NTP responses. On rp2350 and
// bmm-pico the solver runs on core 1 alongside cpu_worker_task, which
// needs scheduling slots to consume produced shares. 100 iters yields
// roughly every 10 ms on the slower cores, satisfying both cases.
const SOLVER_YIELD_INTERVAL: u32 = 100;

/// Worker to `cpu_miner_task`: latest work to mine. Watch semantics mean
/// a fresh frame supersedes whatever the miner is currently working on.
pub static WORK_WATCH: Watch<CriticalSectionRawMutex, WorkFrame, 1> = Watch::new();

/// `cpu_miner_task` to worker: shares produced for the in-flight work.
/// Capacity > 1 covers the brief window where the miner has produced a
/// share before the worker has snapshotted the next frame.
pub static SHARE_CHANNEL: Channel<CriticalSectionRawMutex, RawShare, 4> = Channel::new();

/// `AsicInterface` backed by `WORK_WATCH` and `SHARE_CHANNEL`. Pairs with
/// `cpu_miner_task`; together they preserve the previous in-process CPU
/// mining behaviour while running on the same generic worker code path
/// that a UART ASIC will use.
pub struct ChannelAsic {
    work_sender: embassy_sync::watch::Sender<'static, CriticalSectionRawMutex, WorkFrame, 1>,
    share_receiver: embassy_sync::channel::Receiver<'static, CriticalSectionRawMutex, RawShare, 4>,
}

impl ChannelAsic {
    /// Construct a `ChannelAsic` bound to the module-level statics.
    pub fn new() -> Self {
        Self {
            work_sender: WORK_WATCH.sender(),
            share_receiver: SHARE_CHANNEL.receiver(),
        }
    }
}

impl Default for ChannelAsic {
    fn default() -> Self {
        Self::new()
    }
}

impl AsicInterface for ChannelAsic {
    type Error = Infallible;

    async fn send_work(&mut self, work: &WorkFrame) -> Result<(), Self::Error> {
        self.work_sender.send(*work);
        Ok(())
    }

    async fn recv_share(&mut self) -> Result<RawShare, Self::Error> {
        Ok(self.share_receiver.receive().await)
    }
}

/// Build the 80-byte block header.
///
/// The block header wants `version`/`ntime`/`nbits` serialized little-endian
/// and `prev_hash` with each 4-byte word's byte order reversed. `merkle_root`
/// is already in internal byte order from our local double-SHA256 so it goes
/// in raw. The caller converts stratum big-endian hex to `u32` once during
/// notify parsing so the hot path only performs byte-layout work.
#[must_use]
pub fn build_header(
    version: u32,
    prev_hash: [u8; 32],
    merkle_root: [u8; 32],
    ntime: u32,
    nbits: u32,
    nonce: u32,
) -> [u8; 80] {
    let mut header = [0u8; 80];

    header[0..4].copy_from_slice(&version.to_le_bytes());

    // prev_hash: reverse bytes within each 4-byte word.
    for word_index in 0..8 {
        let source = &prev_hash[word_index * 4..word_index * 4 + 4];
        let dest = &mut header[4 + word_index * 4..4 + word_index * 4 + 4];
        dest[0] = source[3];
        dest[1] = source[2];
        dest[2] = source[1];
        dest[3] = source[0];
    }

    header[36..68].copy_from_slice(&merkle_root);
    header[68..72].copy_from_slice(&ntime.to_le_bytes());
    header[72..76].copy_from_slice(&nbits.to_le_bytes());
    header[76..80].copy_from_slice(&nonce.to_le_bytes());
    header
}

/// Convert a pool-supplied stratum difficulty to a 256-bit little-endian
/// target suitable for byte-by-byte hash comparison.
///
/// `target` = `truediffone` / `diff`, where `truediffone` is
/// `0xFFFF * 2^208`. f64 precision is ~52 bits so the lowest bytes of
/// the target may be slightly off for exotic diffs, but the high bytes
/// that actually decide share acceptance are accurate.
#[must_use]
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub fn target_from_diff(diff: f64) -> [u8; 32] {
    if diff <= 0.0 {
        return [0xFFu8; 32];
    }
    // 2^208 as an exact IEEE-754 double (exponent 208 + bias 1023 = 1231).
    // Avoids relying on libm's powi in no_std builds.
    let pow2_208 = f64::from_bits(0x4CF0_0000_0000_0000);
    let truediff_one = 65_535.0_f64 * pow2_208;
    let mut value = truediff_one / diff;
    let mut target = [0u8; 32];
    for byte in &mut target {
        if value < 1.0 {
            break;
        }
        let low = (value % 256.0) as u8;
        *byte = low;
        // Subtract the low byte before dividing so no libm floor() is needed.
        value = (value - f64::from(low)) / 256.0;
    }
    if value >= 1.0 {
        return [0xFFu8; 32];
    }
    target
}

/// Return `true` when `hash` (little-endian 256-bit integer) is less
/// than or equal to `target`. Exact byte-by-byte compare from MSB down.
#[must_use]
pub fn hash_meets_target(hash: &[u8; 32], target: &[u8; 32]) -> bool {
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

/// Double-SHA256 of the 80-byte block header produced by `build_header`.
#[must_use]
pub fn header_hash(
    version: u32,
    prev_hash: [u8; 32],
    merkle_root: [u8; 32],
    ntime: u32,
    nbits: u32,
    nonce: u32,
) -> [u8; 32] {
    let header = build_header(version, prev_hash, merkle_root, ntime, nbits, nonce);
    let mut out = [0u8; 32];
    double_sha256(&header, &mut out);
    out
}

/// Scan nonces starting at the value already encoded in `header[76..80]`,
/// returning the first nonce whose double-SHA256 meets `target`. Yields
/// every `SOLVER_YIELD_INTERVAL` iterations so the executor can serve
/// other tasks; bumps `total_hashes` in the same batches.
pub async fn mine_share_header(
    mut header: [u8; 80],
    target: [u8; 32],
    total_hashes: &portable_atomic::AtomicU64,
) -> u32 {
    let start_nonce = u32::from_le_bytes([header[76], header[77], header[78], header[79]]);
    let mut nonce = start_nonce;
    let mut result_hash = [0u8; 32];
    loop {
        header[76..80].copy_from_slice(&nonce.to_le_bytes());
        double_sha256(&header, &mut result_hash);
        if hash_meets_target(&result_hash, &target) {
            return nonce;
        }
        nonce = nonce.wrapping_add(1);
        if nonce.is_multiple_of(SOLVER_YIELD_INTERVAL) {
            embassy_futures::yield_now().await;
            total_hashes.fetch_add(u64::from(SOLVER_YIELD_INTERVAL), Ordering::Relaxed);
        }
    }
}

/// CPU-side mining loop. Reads the latest `WorkFrame` from `WORK_WATCH`,
/// runs `mine_share_header` on it, and pushes shares to `SHARE_CHANNEL`.
/// A new frame in the watch aborts the in-flight search via `select`.
#[embassy_executor::task]
pub async fn cpu_miner_task(total_hashes: &'static portable_atomic::AtomicU64) -> ! {
    // cpu-miner is the sole receiver of WORK_WATCH and the watch capacity
    // is 1, so the receiver slot is always free.
    let mut work_receiver = WORK_WATCH
        .receiver()
        .expect("BUG: WORK_WATCH receiver capacity exhausted");
    let mut work = work_receiver.changed().await;
    loop {
        match select(
            mine_share_header(work.header, work.target, total_hashes),
            work_receiver.changed(),
        )
        .await
        {
            Either::First(nonce) => {
                // Channel full means the worker is behind. Drop rather
                // than block: worker resends fresh work next round.
                let _ = SHARE_CHANNEL.try_send(RawShare {
                    nonce,
                    work_id: work.work_id,
                });
                let next_nonce = nonce.wrapping_add(1);
                work.header[76..80].copy_from_slice(&next_nonce.to_le_bytes());
            }
            Either::Second(fresh) => {
                work = fresh;
            }
        }
    }
}

fn double_sha256(data: &[u8], out: &mut [u8; 32]) {
    let mut hasher = Sha256::new();
    hasher.update(data);
    let first = hasher.finalize();

    let mut hasher2 = Sha256::new();
    hasher2.update(first);
    out.copy_from_slice(&hasher2.finalize());
}
