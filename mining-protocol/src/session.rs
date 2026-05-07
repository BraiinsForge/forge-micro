//! Mid-session reconfiguration events.
//!
//! These flow through `visit_session_update` whenever the pool changes
//! something that is *not* a job — target, extranonce, version mask,
//! or a reconnect request. The same variants are also used during
//! initial session setup (the visitor sees a sequence of
//! `SessionUpdate` events before the first `visit_new_job`).

use crate::job_id::JobId;

/// Pool-driven session reconfiguration.
///
/// Borrowed slices are valid only for the duration of the `accept()`
/// call that produced the event; the visitor copies anything it needs
/// to keep.
#[derive(Debug, Clone, Copy)]
pub enum SessionUpdate<'a> {
    /// New share target, 256-bit little-endian.
    SetTarget(&'a [u8; 32]),
    /// New extranonce assignment. V1 emits this on the `subscribe`
    /// response and on `mining.set_extranonce`; V2 emits it during
    /// the channel-open success and (for extended channels) on
    /// `SetExtranoncePrefix`.
    SetExtranonce { prefix: &'a [u8], en2_size: u8 },
    /// New version-rolling mask. V1-only post-handshake; V2 derives
    /// it once from `SetupConnection.flags`.
    SetVersionMask(u32),
    /// Pool requests the client reconnect to a different endpoint.
    Reconnect {
        host: &'a str,
        port: u16,
        wait_seconds: u16,
    },
}

/// Result of a previously submitted `Share`, surfaced to the visitor
/// via `visit_submit_result`.
///
/// Wire-form error details (V1 stratum error array, V2 error code)
/// are normalized into a single `error_code: Option<u32>`. Adapters
/// that want to log richer detail can do so before invoking the
/// visitor — only the structured outcome crosses the trait boundary.
#[derive(Debug, Clone, Copy)]
pub struct SubmitResult<'a> {
    pub id: JobId,
    pub accepted: bool,
    pub error_code: Option<u32>,
    pub error_message: Option<&'a str>,
}
