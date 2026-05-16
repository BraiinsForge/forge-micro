//! `StratumV1Client` — implements `mining_protocol::MiningClient`
//! by wrapping the wire layer in `crate::wire`.
//!
//! See `boards/eth-examples/IMPLEMENTATION_PLAN.md` and
//! `boards/eth-examples/ARCHITECTURE.md` for the surrounding design.

use core::fmt::Write as _;

use embedded_io_async::{Read, Write};
use heapless::String;

use mining_protocol::{
    Error, JobId, LazyMerkle, MiningClient, MiningClientVisitor, MiningJob, SessionRejection,
    SessionUpdate, Share, SubmitResult, MAX_EXTRANONCE_2,
};
use ubos_logging::{info, warn};

use crate::config::Sv1Config;
use crate::scratch::{DecodeScratch, MAX_COINBASE_PART, MAX_MERKLE_BRANCHES};
use crate::state::{
    JobIdSlot, PendingRequestKind, SessionState, JOBID_MAP_DEPTH, MAX_JOB_ID_CHARS,
};
use crate::target::target_from_diff;
use crate::wire::line_buffer::LineBuffer;
use crate::wire::messages::{
    encode_authorize, encode_configure, encode_submit, encode_subscribe, Authorize, Configure,
    Notify, Submit, Subscribe,
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
/// Matches the value `ubos-backend` settled on; public pools push
/// notifies with merkle paths that fit comfortably here.
pub const WIRE_BUFFER_BYTES: usize = 4096;

/// Bytes reserved for one outgoing message before it hits the
/// transport's buffer. Sized for the largest expected request
/// (`mining.submit`) with headroom.
pub const ENCODE_BUFFER_BYTES: usize = 512;

/// Version-rolling mask requested in `mining.configure`. `1fffe000`
/// is the standard mask Bitmain miners use; `min-bit-count: 2`
/// signals the pool we accept any pool-side reduction down to two
/// bits. Pools that don't support the extension simply omit
/// `version-rolling.mask` from their reply, leaving the negotiated
/// mask at zero.
const CONFIGURE_MAP: &[u8] = br#"{"version-rolling":{"mask":"1fffe000","min-bit-count":2}}"#;

/// Bytes reserved for the `result` slice copied out of a handshake
/// response. Public pools' configure / subscribe results stay well
/// under this; ckpool emits ~80 bytes.
const HANDSHAKE_RESULT_BYTES: usize = 256;

/// Stratum V1 protocol adapter.
///
/// Owns the wire buffer, decode scratch, and persistent session
/// state. Constructed once per session; the firmware drops it and
/// builds a fresh one on reconnect.
pub struct StratumV1Client {
    config: &'static Sv1Config,
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

impl StratumV1Client {
    /// Construct an adapter ready for `connect`.
    #[must_use]
    pub const fn new(config: &'static Sv1Config) -> Self {
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

impl MiningClient for StratumV1Client {
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
        let agent = self.config.user;
        self.send_now(transport, |buf| {
            let subscribe = Subscribe {
                agent_signature: Some(agent),
                extra_nonce_1: None,
            };
            encode_subscribe(buf, subscribe_id, &subscribe)
        })
        .await?;

        let authorize_id = self.session.pending.register(PendingRequestKind::Authorize);
        let user = self.config.user;
        let password = self.config.password;
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
        let wire_borrow = lookup_wire_job_id(&self.session, share.id).ok_or(Error::UnknownJobId)?;
        wire_job_id
            .push_str(wire_borrow)
            .map_err(|()| Error::Adapter)?;

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
            user_name: self.config.user,
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
                    self.session.version_rolling_mask = set_mask.mask;
                    info!("stratum-v1: set-version-mask {:#010x}", set_mask.mask,);
                    visitor.visit_session_update(SessionUpdate::SetVersionMask(set_mask.mask));
                }
                Rpc::Notification(Notification::SetExtranonce(set_extranonce)) => {
                    if usize::from(set_extranonce.extra_nonce_2_size) > MAX_EXTRANONCE_2 {
                        warn!(
                            "stratum-v1: set-extranonce en2_size={} > MAX_EXTRANONCE_2={}; ending session",
                            set_extranonce.extra_nonce_2_size,
                            MAX_EXTRANONCE_2,
                        );
                        self.state = V1State::Rejected(SessionRejection::ExtranonceTooLarge);
                    } else if store_extranonce_1(&mut self.session, set_extranonce.extra_nonce_1)
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
        let prev_hex = mining_protocol::hex::encode_block_hash_lower(&decode_scratch.prev_hash);
        info!(
            "stratum-v1: notify job_id={} version={:#010x} nbits={:#010x} \
             ntime={:#010x} clean_jobs={} prev_hash={}",
            notify.job_id,
            version,
            nbits,
            ntime,
            notify.clean_jobs,
            // mining_protocol::hex::encode_lower_to_slice emits lowercase
            // hex (0-9, a-f), which is valid ASCII.
            core::str::from_utf8(&prev_hex).expect("BUG: hex encoder emits ASCII"),
        );
    }

    // Allocate a JobId and record the (generic, wire) mapping. Slot
    // rotation matches the depth-2 ring on the stratum-client side.
    let job_id = allocate_job_id(session, notify.job_id);

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
        version_rolling_mask: session.version_rolling_mask,
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

/// Translate a generic `JobId` back to the V1 wire-form string.
/// Returns `None` if the `JobId` has been evicted from the depth-2
/// ring.
fn lookup_wire_job_id(session: &SessionState, generic: JobId) -> Option<&str> {
    session
        .job_id_ring
        .iter()
        .filter_map(|slot| slot.as_ref())
        .find(|slot| slot.generic == generic)
        .map(|slot| slot.wire.as_str())
}

/// Push the (`JobId`, wire `job_id`) pair onto the depth-2 ring.
fn allocate_job_id(session: &mut SessionState, wire_id: &str) -> JobId {
    let generic = JobId(session.next_job_id);
    session.next_job_id = session.next_job_id.wrapping_add(1);

    let mut wire = String::<MAX_JOB_ID_CHARS>::new();
    let truncated = if wire_id.len() > MAX_JOB_ID_CHARS {
        // Keep the prefix; longer wire ids indicate a misbehaving
        // pool but we don't want to fail the job over it.
        &wire_id[..MAX_JOB_ID_CHARS]
    } else {
        wire_id
    };
    let _ = wire.push_str(truncated);

    let index = session.job_id_write_index;
    session.job_id_ring[index] = Some(JobIdSlot { generic, wire });
    session.job_id_write_index = (index + 1) % JOBID_MAP_DEPTH;

    generic
}

fn store_extranonce_1(session: &mut SessionState, hex: &str) -> bool {
    let hex_bytes = hex.as_bytes();
    if hex_bytes.len() / 2 > session.extranonce_1.len() {
        return false;
    }
    let len = hex_bytes.len() / 2;
    if !decode_hex(hex_bytes, &mut session.extranonce_1[..len]) {
        return false;
    }
    session.extranonce_1_len = len;
    true
}

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
            if let Some(result) = response.stratum_result {
                if let Some(mask) = parse_version_rolling_mask(result) {
                    session.version_rolling_mask = mask;
                    visitor.visit_session_update(SessionUpdate::SetVersionMask(mask));
                }
            }
            None
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
            if !store_extranonce_1(session, parsed.1) {
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
