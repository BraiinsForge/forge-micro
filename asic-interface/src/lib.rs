//! Trait surface between a stratum-side worker and any ASIC backend.
//!
//! The worker hands the ASIC a `WorkFrame` and waits for a `RawShare`.
//! Implementations live with the backend they wrap: `cpu-miner` for the
//! in-process CPU miner, `eth-examples::uart_asic` for a UART-attached
//! external ASIC. Keeping the trait in its own crate lets either side
//! evolve without dragging the Stratum client or the SHA-256 inner loop
//! behind it.

#![no_std]

/// Header and target pair pushed to an ASIC for one mining round.
#[derive(Clone, Copy)]
pub struct WorkFrame {
    /// Serialized 80-byte block header with the starting nonce pre-stamped
    /// into bytes 76–79 by the worker.
    pub header: [u8; 80],
    /// Pool difficulty target in 256-bit little-endian form.
    pub target: [u8; 32],
    /// Worker-assigned identifier for this stratum job. Bumped on every
    /// job change so a backend that decouples mining from share consumption
    /// (channel- or UART-based) can tag the shares it produces; the worker
    /// drops shares whose `work_id` does not match the in-flight job.
    pub work_id: u32,
}

/// Share returned by an ASIC. The worker retains all header fields; the
/// ASIC only needs to report the winning nonce.
#[derive(Clone, Copy)]
pub struct RawShare {
    /// Winning nonce value in host byte order.
    pub nonce: u32,
    /// `work_id` of the `WorkFrame` this share was produced for. Lets the
    /// worker discard a share that was already in flight when a fresh job
    /// landed and would otherwise be packaged with the wrong stratum
    /// `job_id`/`extranonce2`/`ntime`.
    pub work_id: u32,
}

/// Pluggable mining backend.
// async_fn_in_trait: implementations run on a cooperative single-executor;
// Send bounds on the returned futures are neither needed nor possible
// in no_std.
#[allow(async_fn_in_trait)]
pub trait AsicInterface {
    /// Error type surfaced by send/receive operations.
    type Error: defmt::Format;

    /// Push a new work frame to the backend. The call returns once the
    /// frame has been accepted; it does not wait for a share.
    async fn send_work(&mut self, work: &WorkFrame) -> Result<(), Self::Error>;

    /// Block until the backend produces a share for the most recently
    /// submitted work frame.
    async fn recv_share(&mut self) -> Result<RawShare, Self::Error>;
}
