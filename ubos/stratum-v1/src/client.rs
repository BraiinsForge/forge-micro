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

//! `StratumV1Client` — implements `mining_protocol::MiningClient`
//! by wrapping the wire layer in `crate::wire`.
//!
//! The dispatch functions below are free functions, and each takes three
//! or four parameters that are peers rather than a receiver and its
//! arguments. That is not a missed `&mut self`. A parsed message borrows
//! out of `self.wire_buffer`, so any method on the client would hold the
//! whole of `self` and collide with the message it was handed. The
//! caller therefore splits `self` into the disjoint fields each
//! dispatch needs and passes them separately. Read a long parameter list
//! here as that split, and keep it when changing one.
//!
//! Architecture: `../../../docs/STRATUM_ARCHITECTURE.md`.
//! Design rationale: `../../../docs/plan/STRATUM_IMPLEMENTATION_PLAN.md`.

use core::fmt::Write as _;

use embedded_io_async::{Read, Write};
use heapless::String;

use mining_protocol::{
    Error, LazyMerkle, MAX_EXTRANONCE_2, MiningClient, MiningClientVisitor, MiningJob,
    SessionRejection, SessionUpdate, Share, SubmitResult,
};
use ubos_logging::{info, warn};

use crate::config::Sv1Config;
use crate::scratch::{DecodeScratch, MAX_COINBASE_PART, MAX_MERKLE_BRANCHES};
use crate::state::{MAX_JOB_ID_CHARS, PendingRequestKind, SessionState};
use crate::target::target_from_diff;
use crate::wire::line_buffer::LineBuffer;
use crate::wire::messages::{
    Authorize, Configure, Notify, Submit, Subscribe, encode_authorize, encode_configure,
    encode_submit, encode_subscribe,
};
use crate::wire::parse::{
    parse_boolean_result, parse_incoming, parse_subscribe_result, parse_version_rolling_mask,
};
use crate::wire::rpc::{Notification, Response, Rpc, StratumError};
use mining_protocol::hex::decode_to_slice as decode_hex;

#[cfg(test)]
#[path = "tests/client.rs"]
mod tests;

/// Bytes reserved for incoming wire frames (one JSON-RPC line).
/// Public pools push notifies with merkle paths that fit comfortably
/// here.
pub const WIRE_BUFFER_BYTES: usize = 4096;

/// Bytes reserved for one outgoing message before it hits the
/// transport's buffer. Sized for the largest expected request
/// (`mining.submit`) with headroom.
pub const ENCODE_BUFFER_BYTES: usize = 512;

/// Version-rolling parameters sent in `mining.configure`. `1fffe000`
/// is the standard mask Bitmain miners use; `min-bit-count: 2`
/// signals the pool we accept any pool-side reduction down to two
/// bits. The keys are the flat dotted names BIP310 defines, because
/// some pools refuse version rolling when they are nested. A reply
/// without `version-rolling.mask` ends the session as
/// `SessionRejection::VersionRollingRefused`.
const CONFIGURE_MAP: &[u8] =
    br#"{"version-rolling.mask":"1fffe000","version-rolling.min-bit-count":2}"#;

/// Bytes reserved for the `result` slice copied out of a handshake
/// response. Public pools' configure / subscribe results stay well
/// under this; the largest we have observed is around 80 bytes.
const HANDSHAKE_RESULT_BYTES: usize = 256;

/// Stratum V1 protocol adapter.
///
/// Owns the wire buffer, decode scratch, and persistent session
/// state. Constructed once per session; the firmware drops it and
/// builds a fresh one on reconnect.
pub struct StratumV1Client<'cfg> {
    config: &'cfg Sv1Config,
    wire_buffer: LineBuffer<WIRE_BUFFER_BYTES>,
    decode_scratch: DecodeScratch,
    session: SessionState,
    encode_buffer: [u8; ENCODE_BUFFER_BYTES],
    /// Bytes pending flush in `encode_buffer`. `0` when nothing is
    /// queued.
    outbound_len: usize,
    /// Cursor into `encode_buffer[..outbound_len]` for resuming a
    /// partially-written flush after a `drive()` cancellation.
    outbound_offset: usize,
    /// Lifecycle phase. `Active` for the full session; `Rejected`
    /// after the pool refuses subscribe or authorize. The next
    /// `drive` / `submit` then returns
    /// `Error::SessionRejected(cause)` so the work loop tears the
    /// connection down and reconnects.
    state: V1State,
}

#[derive(Clone, Copy)]
enum V1State {
    Active,
    Rejected(SessionRejection),
}

impl<'cfg> StratumV1Client<'cfg> {
    /// Construct an adapter ready for `connect`.
    #[must_use]
    pub const fn new(config: &'cfg Sv1Config) -> Self {
        Self {
            config,
            wire_buffer: LineBuffer::new(),
            decode_scratch: DecodeScratch::new(),
            session: SessionState::new(),
            encode_buffer: [0; ENCODE_BUFFER_BYTES],
            outbound_len: 0,
            outbound_offset: 0,
            state: V1State::Active,
        }
    }

    /// Flush any queued outbound bytes to `transport`.
    ///
    /// Resumable: tracks `outbound_offset` so a cancelled `drive`
    /// future doesn't re-send already-written bytes when `drive` is
    /// next called.
    async fn flush_outbound<T: Write>(&mut self, transport: &mut T) -> Result<(), Error> {
        while self.outbound_offset < self.outbound_len {
            let written = transport
                .write(&self.encode_buffer[self.outbound_offset..self.outbound_len])
                .await
                .map_err(|_| Error::Transport)?;
            if written == 0 {
                return Err(Error::Transport);
            }
            self.outbound_offset += written;
        }
        self.outbound_len = 0;
        self.outbound_offset = 0;
        Ok(())
    }

    /// Send one message synchronously: encode into `encode_buffer`,
    /// write to `transport`. Used during the handshake where we
    /// pipeline configure/subscribe/authorize before returning from
    /// `connect`. After connect, queued outbound goes through
    /// `outbound_len` and `flush_outbound`.
    async fn send_now<T: Write, F>(&mut self, transport: &mut T, encode: F) -> Result<(), Error>
    where
        F: FnOnce(&mut [u8]) -> Result<usize, crate::error::EncodeError>,
    {
        let length = encode(&mut self.encode_buffer).map_err(|_| Error::BufferTooSmall)?;
        let mut offset = 0;
        while offset < length {
            let written = transport
                .write(&self.encode_buffer[offset..length])
                .await
                .map_err(|_| Error::Transport)?;
            if written == 0 {
                return Err(Error::Transport);
            }
            offset += written;
        }
        Ok(())
    }

    /// Test-only access to the pending-request map so unit tests can
    /// stage a response without driving the full `connect` handshake
    /// against an in-memory transport.
    #[cfg(test)]
    pub(crate) fn register_pending(&mut self, kind: PendingRequestKind) -> u32 {
        self.session.pending.register(kind)
    }
}

impl MiningClient for StratumV1Client<'_> {
    /// V1 has no transport-level handshake (no Noise, no certificate
    /// validation). `connect` pipelines the three handshake
    /// requests — `mining.configure`, `mining.subscribe`,
    /// `mining.authorize` — and returns. Their responses arrive over
    /// the regular `drive`/`accept` loop, where the visitor sees
    /// them as `SessionUpdate` events.
    async fn connect<T>(&mut self, transport: &mut T) -> Result<(), Error>
    where
        T: Read + Write,
    {
        let configure_id = self.session.pending.register(PendingRequestKind::Configure);
        self.send_now(transport, |buf| {
            let configure = Configure {
                features: &["version-rolling"],
                configure_map: CONFIGURE_MAP,
            };
            encode_configure(buf, configure_id, &configure)
        })
        .await?;

        let subscribe_id = self.session.pending.register(PendingRequestKind::Subscribe);
        let agent = self.config.user.as_str();
        self.send_now(transport, |buf| {
            let subscribe = Subscribe {
                agent_signature: Some(agent),
                extra_nonce_1: None,
            };
            encode_subscribe(buf, subscribe_id, &subscribe)
        })
        .await?;

        let authorize_id = self.session.pending.register(PendingRequestKind::Authorize);
        let user = self.config.user.as_str();
        let password = self.config.password.as_str();
        self.send_now(transport, |buf| {
            let authorize = Authorize {
                name: user,
                password,
            };
            encode_authorize(buf, authorize_id, &authorize)
        })
        .await?;

        Ok(())
    }

    /// Encode a `mining.submit` request into the outbound slot.
    /// Returns `Err(Error::UnknownJobId)` if `share.id` no longer
    /// has a wire-form mapping (the `JobId` ring rotated past it),
    /// `Err(Error::BufferTooSmall)` if the outbound slot is busy.
    /// The next `drive()` flushes the encoded bytes.
    fn submit(&mut self, share: &Share<'_>) -> Result<(), Error> {
        if let V1State::Rejected(cause) = self.state {
            return Err(Error::SessionRejected(cause));
        }
        if self.outbound_len > 0 {
            return Err(Error::BufferTooSmall);
        }

        // Copy the wire-form job_id out of the ring so the immutable
        // borrow ends before we mutate session for the submit id.
        let mut wire_job_id = String::<MAX_JOB_ID_CHARS>::new();
        let wire_borrow = self
            .session
            .wire_job_id(share.id)
            .ok_or(Error::UnknownJobId)?;
        wire_job_id
            .push_str(wire_borrow)
            .map_err(|_| Error::Adapter)?;

        let pool_difficulty = mining_protocol::difficulty::approx_value(&self.session.target);
        let submit_id = self
            .session
            .pending
            .register(PendingRequestKind::Submit { pool_difficulty });

        let mut extranonce_2_hex = String::<{ crate::scratch::MAX_EXTRANONCE_1 * 2 }>::new();
        for byte in share.extranonce_2 {
            let _ = write!(&mut extranonce_2_hex, "{byte:02x}");
        }
        let mut ntime_hex = String::<8>::new();
        let _ = write!(&mut ntime_hex, "{:08x}", share.ntime);
        let mut nonce_hex = String::<8>::new();
        let _ = write!(&mut nonce_hex, "{:08x}", share.nonce);
        let mut version_hex = String::<8>::new();
        let _ = write!(&mut version_hex, "{:08x}", share.version_bits);

        let message = Submit {
            user_name: self.config.user.as_str(),
            job_id: wire_job_id.as_str(),
            extra_nonce_2: extranonce_2_hex.as_str(),
            time: ntime_hex.as_str(),
            nonce: nonce_hex.as_str(),
            version: version_hex.as_str(),
        };
        let length = encode_submit(&mut self.encode_buffer, submit_id, &message)
            .map_err(|_| Error::BufferTooSmall)?;
        self.outbound_len = length;
        self.outbound_offset = 0;
        info!(
            "stratum-v1: submit queued id={} job_id={} \
             nonce={:#010x} ntime={:#010x} version_bits={:#010x}",
            submit_id,
            wire_job_id.as_str(),
            share.nonce,
            share.ntime,
            share.version_bits,
        );
        Ok(())
    }

    /// One drive call: flush any queued outbound bytes, then read
    /// from `transport` until at least one complete line is buffered
    /// in `wire_buffer`.
    ///
    /// Cancellation safety: `LineBuffer` accumulates incrementally,
    /// `outbound_offset` tracks partial writes, so a cancelled
    /// future leaves the adapter ready to resume.
    async fn drive<T>(&mut self, transport: &mut T) -> Result<(), Error>
    where
        T: Read + Write,
    {
        if let V1State::Rejected(cause) = self.state {
            return Err(Error::SessionRejected(cause));
        }
        self.flush_outbound(transport).await?;

        loop {
            let line_present = self
                .wire_buffer
                .next_line()
                .map_err(|_| Error::Decode)?
                .is_some();
            if line_present {
                return Ok(());
            }

            let writable = self.wire_buffer.writable();
            if writable.is_empty() {
                return Err(Error::Decode);
            }
            let read_count = transport
                .read(writable)
                .await
                .map_err(|_| Error::Transport)?;
            if read_count == 0 {
                return Err(Error::Transport);
            }
            self.wire_buffer.advance(read_count);
        }
    }

    /// Dispatch the most recently buffered line to `visitor`.
    ///
    /// Borrowed slices handed to the visitor point into either the
    /// wire buffer (V2-style direct borrow) or the decode scratch
    /// (V1's hex-decoded binary). Both buffers belong to `self`, so
    /// the visitor's lifetime is bound to this call's `&mut self`.
    /// Once `accept` returns, the wire buffer is freed for the next
    /// `drive` to reuse.
    #[allow(clippy::too_many_lines)]
    fn accept<V: MiningClientVisitor>(&mut self, visitor: &mut V) {
        // Run the dispatch in its own scope so the borrows on
        // `self.wire_buffer` (via `rpc`) end before `consume()`.
        {
            let Some(line) = self.wire_buffer.next_line().ok().flatten() else {
                return;
            };

            let Ok(rpc) = parse_incoming(line) else {
                // Drop the unparseable line so `drive` doesn't
                // re-see it. Fall through to the consume below.
                let _ = line;
                self.wire_buffer.consume();
                return;
            };

            match rpc {
                Rpc::Notification(Notification::Notify(notify)) => {
                    dispatch_notify(
                        &notify,
                        &mut self.decode_scratch,
                        &mut self.session,
                        visitor,
                    );
                }
                Rpc::Notification(Notification::SetDifficulty(set_diff)) => {
                    self.session.target = target_from_diff(set_diff.0);
                    {
                        let target_hex =
                            mining_protocol::hex::encode_block_hash_lower(&self.session.target);
                        info!(
                            "stratum-v1: set-difficulty diff={} target={}...",
                            set_diff.0,
                            // mining_protocol::hex::encode_block_hash_lower emits lowercase
                            // hex (0-9, a-f), which is valid ASCII.
                            core::str::from_utf8(&target_hex[..32])
                                .expect("BUG: hex encoder emits ASCII"),
                        );
                    }
                    visitor.visit_session_update(SessionUpdate::SetTarget(&self.session.target));
                }
                Rpc::Notification(Notification::SetVersionMask(set_mask)) => {
                    info!("stratum-v1: set-version-mask {:#010x}", set_mask.mask,);
                    visitor.visit_session_update(SessionUpdate::SetVersionMask(set_mask.mask));
                }
                Rpc::Notification(Notification::SetExtranonce(set_extranonce)) => {
                    if usize::from(set_extranonce.extra_nonce_2_size) > MAX_EXTRANONCE_2 {
                        warn!(
                            "stratum-v1: set-extranonce en2_size={} > MAX_EXTRANONCE_2={}; ending session",
                            set_extranonce.extra_nonce_2_size, MAX_EXTRANONCE_2,
                        );
                        self.state = V1State::Rejected(SessionRejection::ExtranonceTooLarge);
                    } else if self
                        .session
                        .store_extranonce_1(set_extranonce.extra_nonce_1)
                        && set_extranonce.extra_nonce_2_size as usize
                            <= crate::scratch::MAX_EXTRANONCE_1
                    {
                        self.session.extranonce_2_size = set_extranonce.extra_nonce_2_size;
                        let prefix = &self.session.extranonce_1[..self.session.extranonce_1_len];
                        let mut prefix_hex = [0u8; crate::scratch::MAX_EXTRANONCE_1 * 2];
                        let prefix_hex_len = prefix.len() * 2;
                        let prefix_hex_ok = mining_protocol::hex::encode_lower_to_slice(
                            prefix,
                            &mut prefix_hex[..prefix_hex_len],
                        );
                        debug_assert!(prefix_hex_ok);
                        info!(
                            "stratum-v1: set-extranonce prefix={} en2_size={}",
                            // mining_protocol::hex::encode_lower_to_slice emits lowercase
                            // hex (0-9, a-f), which is valid ASCII.
                            core::str::from_utf8(&prefix_hex[..prefix_hex_len])
                                .expect("BUG: hex encoder emits ASCII"),
                            self.session.extranonce_2_size,
                        );
                        visitor.visit_session_update(SessionUpdate::SetExtranonce {
                            prefix,
                            en2_size: self.session.extranonce_2_size,
                        });
                    }
                }
                Rpc::Notification(Notification::ClientReconnect(reconnect)) => {
                    info!(
                        "stratum-v1: client-reconnect host={} port={} wait={}",
                        reconnect.host, reconnect.port, reconnect.wait,
                    );
                    visitor.visit_session_update(SessionUpdate::Reconnect {
                        host: reconnect.host,
                        port: reconnect.port,
                        wait_seconds: reconnect.wait,
                    });
                }
                Rpc::Notification(Notification::Ping { id }) => {
                    queue_ping_response(
                        &mut self.encode_buffer,
                        &mut self.outbound_len,
                        &mut self.outbound_offset,
                        id,
                    );
                }
                Rpc::Response(response) => {
                    if let Some(rejection) =
                        dispatch_response(&response, &mut self.session, visitor)
                    {
                        self.state = V1State::Rejected(rejection);
                    }
                }
            }
        }

        self.wire_buffer.consume();
    }
}

/// Turn a `mining.notify` into a [`MiningJob`] and hand it to `visitor`.
///
/// Decodes the hex fields into `decode_scratch`, applies the V1
/// `prev_hash` word swap so the trait surface sees canonical
/// block-header bytes, allocates a [`JobId`] against `session`, then
/// calls the visitor once.
///
/// A malformed field drops the job and returns without telling the
/// visitor. That is deliberate: a pool that sends bad hex has sent no
/// work, and a session that stayed up is better than one that failed
/// over for a single bad line.
///
/// The four parameters are peers and none of them is a receiver. The
/// notify is borrowed from the wire buffer, the scratch and the session
/// are two disjoint pieces of state the caller holds, and the visitor is
/// where the result goes. A method on any one of them would take the
/// other three, so the call site keeps them apart.
fn dispatch_notify<V: MiningClientVisitor>(
    notify: &Notify<'_>,
    decode_scratch: &mut DecodeScratch,
    session: &mut SessionState,
    visitor: &mut V,
) {
    if !load_notify_into_scratch(notify, decode_scratch) {
        // Bad hex somewhere in the notify — drop the job.
        return;
    }

    // Decode the small fixed-size hex fields directly into u32s.
    let Some(version) = decode_hex_u32_be(notify.version) else {
        return;
    };
    let Some(ntime) = decode_hex_u32_be(notify.time) else {
        return;
    };
    let Some(nbits) = decode_hex_u32_be(notify.bits) else {
        return;
    };

    // Apply the V1 prev_hash per-u32 byte swap so the trait surface
    // sees canonical block-header bytes.
    swap_prev_hash_words(&mut decode_scratch.prev_hash);

    {
        const PREV_HASH_TAIL_LEN: usize = 6;
        let prev_hex = mining_protocol::hex::encode_block_hash_lower(&decode_scratch.prev_hash);
        let prev_tail = &prev_hex[prev_hex.len().saturating_sub(PREV_HASH_TAIL_LEN)..];
        info!(
            "stratum-v1: notify job_id={} version={:#010x} nbits={:#010x} \
             ntime={:#010x} clean_jobs={} prev_hash=…{}",
            notify.job_id,
            version,
            nbits,
            ntime,
            notify.clean_jobs,
            // mining_protocol::hex::encode_block_hash_lower emits lowercase
            // hex (0-9, a-f), which is valid ASCII.
            core::str::from_utf8(prev_tail).expect("BUG: hex encoder emits ASCII"),
        );
    }

    // Allocate a JobId and record the (generic, wire) mapping. Slot
    // rotation matches the depth-2 ring on the stratum-client side.
    let job_id = session.allocate_job_id(notify.job_id);

    let extranonce_2_size = session.extranonce_2_size;
    let extranonce_prefix = &session.extranonce_1[..session.extranonce_1_len];
    let coinbase_prefix = &decode_scratch.coinbase_1[..decode_scratch.coinbase_1_len];
    let coinbase_suffix = &decode_scratch.coinbase_2[..decode_scratch.coinbase_2_len];
    let branches = &decode_scratch.branches[..decode_scratch.branch_count];

    let job = MiningJob {
        id: job_id,
        prev_hash: &decode_scratch.prev_hash,
        target: &session.target,
        version,
        nbits,
        ntime,
        clean_jobs: notify.clean_jobs,
        extranonce_2_size,
        merkle: LazyMerkle::Prepared {
            extranonce_prefix,
            coinbase_prefix,
            coinbase_suffix,
            branches,
        },
    };
    visitor.visit_new_job(job);
}

/// Hex-decode the variable-size pieces of a notify into
/// `decode_scratch`. Returns `false` on any hex error or oversized
/// field.
fn load_notify_into_scratch(notify: &Notify<'_>, decode_scratch: &mut DecodeScratch) -> bool {
    let prev_hash_bytes = notify.prev_hash.as_bytes();
    if !decode_hex(prev_hash_bytes, &mut decode_scratch.prev_hash) {
        return false;
    }

    let coinbase_1_hex = notify.coin_base_1.as_bytes();
    if coinbase_1_hex.len() / 2 > MAX_COINBASE_PART {
        warn!(
            "stratum-v1: notify coinbase_1 len={} > MAX_COINBASE_PART={}; dropping job",
            coinbase_1_hex.len() / 2,
            MAX_COINBASE_PART,
        );
        return false;
    }
    decode_scratch.coinbase_1_len = coinbase_1_hex.len() / 2;
    if !decode_hex(
        coinbase_1_hex,
        &mut decode_scratch.coinbase_1[..decode_scratch.coinbase_1_len],
    ) {
        return false;
    }

    let coinbase_2_hex = notify.coin_base_2.as_bytes();
    if coinbase_2_hex.len() / 2 > MAX_COINBASE_PART {
        warn!(
            "stratum-v1: notify coinbase_2 len={} > MAX_COINBASE_PART={}; dropping job",
            coinbase_2_hex.len() / 2,
            MAX_COINBASE_PART,
        );
        return false;
    }
    decode_scratch.coinbase_2_len = coinbase_2_hex.len() / 2;
    if !decode_hex(
        coinbase_2_hex,
        &mut decode_scratch.coinbase_2[..decode_scratch.coinbase_2_len],
    ) {
        return false;
    }

    if notify.merkle_branch.len() > MAX_MERKLE_BRANCHES {
        return false;
    }
    decode_scratch.branch_count = notify.merkle_branch.len();
    for (slot, branch_hex) in decode_scratch
        .branches
        .iter_mut()
        .zip(notify.merkle_branch.iter())
        .take(decode_scratch.branch_count)
    {
        if !decode_hex(branch_hex.as_bytes(), slot) {
            return false;
        }
    }

    true
}

/// V1 ships `prev_hash` as 32 bytes whose 4-byte words are
/// individually big-endian but laid out in the array in
/// little-endian word order. The block header expects each word in
/// natural (BE-on-the-wire) order, which is the per-u32 byte-swap
/// applied here.
fn swap_prev_hash_words(prev_hash: &mut [u8; 32]) {
    for word in 0..8 {
        let base = word * 4;
        prev_hash.swap(base, base + 3);
        prev_hash.swap(base + 1, base + 2);
    }
}

/// Decode exactly eight hex characters as a big-endian `u32`.
///
/// The length is fixed rather than bounded, because every field this
/// reads — `version`, `time` and `bits` — is a four-byte value the
/// specification requires the pool to send zero-padded. A short field is
/// a pool error and not a small number, so it returns `None` instead of
/// a value the caller would go on to hash.
fn decode_hex_u32_be(text: &str) -> Option<u32> {
    let bytes = text.as_bytes();
    if bytes.len() != 8 {
        return None;
    }
    let mut buf = [0u8; 4];
    if !decode_hex(bytes, &mut buf) {
        return None;
    }
    Some(u32::from_be_bytes(buf))
}

/// Stage the reply to a pool-initiated `mining.ping` in the outbound
/// slot.
///
/// Writes nothing when the slot already holds an unsent message. A
/// dropped ping reply costs nothing, because the pool sends another one
/// if it is watching, and overwriting a pending submit would cost a
/// share.
///
/// The three `&mut` parameters are one outbound slot taken apart: the
/// buffer, the length and the offset. They arrive as three field borrows
/// rather than as `&mut self`, because the parsed message the caller is
/// acting on is itself borrowed out of `self.wire_buffer`. A method on
/// the client would need the whole of `self` and collide with that
/// borrow, so the caller splits it and this takes the pieces.
fn queue_ping_response(
    encode_buffer: &mut [u8; ENCODE_BUFFER_BYTES],
    outbound_len: &mut usize,
    outbound_offset: &mut usize,
    id: u32,
) {
    if *outbound_len > 0 {
        // Outbound slot already in use; drop the ping. The pool
        // re-sends if it really cares.
        return;
    }
    let mut response = String::<64>::new();
    let _ = write!(&mut response, r#"{{"id":{id},"result":null,"error":null}}"#);
    let _ = response.push('\n');
    let bytes = response.as_bytes();
    if bytes.len() > encode_buffer.len() {
        return;
    }
    encode_buffer[..bytes.len()].copy_from_slice(bytes);
    *outbound_len = bytes.len();
    *outbound_offset = 0;
}

/// Match a response to its pending request and apply what it carries.
///
/// Takes the pending entry for `response.id`, reads the result or the
/// error against the request kind that entry records, updates `session`,
/// and reports to `visitor`. Returns `Some` when the response ends the
/// session, which is how an authorize or a subscribe failure reaches the
/// caller as a state change rather than as a log line.
///
/// A response with no pending entry is dropped with one warning. Two
/// things reach that arm: a pool answering something never asked, and a
/// slot at `id % PENDING_DEPTH` evicted by a younger registration. The
/// session is healthy in both cases.
///
/// Like [`dispatch_notify`], the three parameters are peers. The
/// response is borrowed from the wire buffer, the session is the state
/// it updates, and the visitor is where the result goes.
#[allow(clippy::too_many_lines)]
fn dispatch_response<V: MiningClientVisitor>(
    response: &Response<'_>,
    session: &mut SessionState,
    visitor: &mut V,
) -> Option<SessionRejection> {
    let Some(kind) = session.pending.take(response.id) else {
        // No pending request for this wire id. Either the pool
        // sent a response we never asked for, or the slot at
        // `id % N` had been evicted by a younger registration
        // (in-flight load exceeded `PENDING_DEPTH`). Drop after
        // a single warn — the session itself is still healthy.
        warn!(
            "stratum-v1: response for unknown id {}, dropping",
            response.id,
        );
        return None;
    };
    match kind {
        PendingRequestKind::Configure => {
            let mask = response
                .stratum_result
                .and_then(parse_version_rolling_mask)
                .filter(|&mask| mask != 0);
            if let Some(mask) = mask {
                visitor.visit_session_update(SessionUpdate::SetVersionMask(mask));
                return None;
            }
            let error_code = response
                .stratum_error
                .as_ref()
                .map(|StratumError(code, _message, _detail)| *code);
            warn!(
                "stratum-v1: the pool did not grant version rolling, error code {:?}. The session stops.",
                error_code,
            );
            Some(SessionRejection::VersionRollingRefused)
        }
        PendingRequestKind::Subscribe => {
            let result = match response.stratum_result {
                Some(result) if response.stratum_error.is_none() => result,
                _ => {
                    warn!("stratum-v1: subscribe rejected by pool, ending session");
                    return Some(SessionRejection::SubscribeRefused);
                }
            };
            // Copy the result slice out so we can re-parse it
            // without holding the wire-buffer borrow.
            let mut buffer = [0u8; HANDSHAKE_RESULT_BYTES];
            let len = result.len().min(buffer.len());
            buffer[..len].copy_from_slice(&result[..len]);
            let Ok(parsed) = parse_subscribe_result(&buffer[..len]) else {
                return None;
            };
            if usize::from(parsed.2) > MAX_EXTRANONCE_2 {
                warn!(
                    "stratum-v1: subscribe en2_size={} > MAX_EXTRANONCE_2={}; ending session",
                    parsed.2, MAX_EXTRANONCE_2,
                );
                return Some(SessionRejection::ExtranonceTooLarge);
            }
            if !session.store_extranonce_1(parsed.1) {
                return None;
            }
            session.extranonce_2_size = parsed.2;
            let prefix = &session.extranonce_1[..session.extranonce_1_len];
            let mut prefix_hex = [0u8; crate::scratch::MAX_EXTRANONCE_1 * 2];
            let prefix_hex_len = prefix.len() * 2;
            let prefix_hex_ok = mining_protocol::hex::encode_lower_to_slice(
                prefix,
                &mut prefix_hex[..prefix_hex_len],
            );
            debug_assert!(prefix_hex_ok);
            info!(
                "stratum-v1: subscribe-success prefix={} en2_size={}",
                // mining_protocol::hex::encode_lower_to_slice emits lowercase
                // hex (0-9, a-f), which is valid ASCII.
                core::str::from_utf8(&prefix_hex[..prefix_hex_len])
                    .expect("BUG: hex encoder emits ASCII"),
                session.extranonce_2_size,
            );
            visitor.visit_session_update(SessionUpdate::SetExtranonce {
                prefix,
                en2_size: session.extranonce_2_size,
            });
            None
        }
        PendingRequestKind::Authorize => {
            // Authorize is acknowledged; either an error object or
            // a boolean `false` means the pool refused the worker
            // name and no shares from this session will ever be
            // accepted. End the session in either case.
            let accepted = response.stratum_error.is_none()
                && response
                    .stratum_result
                    .and_then(|raw| parse_boolean_result(raw).ok())
                    .unwrap_or(false);
            if !accepted {
                warn!("stratum-v1: authorize rejected by pool, ending session");
                return Some(SessionRejection::AuthorizeRefused);
            }
            None
        }
        PendingRequestKind::Submit { pool_difficulty } => {
            let accepted = response
                .stratum_result
                .and_then(|raw| parse_boolean_result(raw).ok())
                .unwrap_or(false);
            let result = if accepted {
                info!("stratum-v1: submit-accepted work={}", pool_difficulty);
                SubmitResult::Accepted {
                    count: 1,
                    work: pool_difficulty,
                }
            } else {
                let (error_code, error_message) = match &response.stratum_error {
                    Some(StratumError(code, message, _detail)) => {
                        let code_unsigned = u32::try_from(*code).ok();
                        (code_unsigned, Some(*message))
                    }
                    None => (None, None),
                };
                warn!(
                    "stratum-v1: submit-rejected code={:?} message={:?}",
                    error_code, error_message,
                );
                SubmitResult::Rejected {
                    error_code,
                    error_message,
                }
            };
            visitor.visit_submit_result(result);
            None
        }
    }
}
