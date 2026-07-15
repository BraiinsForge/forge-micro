//! `PoolConnector` — the transport-acquisition seam for a stratum
//! session.
//!
//! `run_stratum_client` already treats the transport as an opaque
//! `Read + Write` object; this trait exists one layer up so that
//! *obtaining* that transport (a TCP socket, an in-process pipe, …)
//! is pluggable independent of the protocol dispatch and
//! work-generation loop. Reconnect policy and pool-config dispatch
//! stay with the callers — see [`run_stratum_session`].

use embedded_io_async::{Read, Write};
use mining_protocol::{Error, MiningClient};

use crate::asic_session::PoolHandle;
use crate::run::run_stratum_client;
use crate::state::StratumState;

/// Yields one connected transport per call. Implementations own the
/// address/buffer plumbing (TCP via embassy-net, in-process pipes, …).
///
/// `Conn` is a generic associated type borrowing from the connector —
/// the shape `embedded_nal_async::TcpConnect` established. A TCP
/// implementation hands its socket buffers to `TcpSocket<'a>`, so the
/// connection must be allowed to borrow from `&mut self`; a
/// self-contained implementation (an in-process pipe owning its
/// storage) simply ignores the lifetime.
//
// async_fn_in_trait: implementations run on a cooperative
// single-executor; Send bounds on returned futures are neither
// needed nor possible in no_std. Mirrors `MiningClient`'s rationale.
#[allow(async_fn_in_trait)]
pub trait PoolConnector {
    type Conn<'a>: Read + Write
    where
        Self: 'a;
    type Error: core::fmt::Debug;
    async fn connect(&mut self) -> Result<Self::Conn<'_>, Self::Error>;
}

/// Failure from [`run_stratum_session`]: either the connector could
/// not produce a transport, or a connected session errored.
#[derive(Debug)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum SessionError<E> {
    /// `PoolConnector::connect` failed before any session started.
    Connect(E),
    /// `run_stratum_client` returned an error over a connected transport.
    Session(Error),
}

/// Obtain one connection from `connector`, then run
/// [`run_stratum_client`] over it.
///
/// This function does not own the outer reconnect loop: it makes
/// exactly one connect attempt and runs exactly one session. Callers
/// keep looping, watching config-restart signals, and rebuilding
/// protocol clients and connectors per iteration — unchanged from
/// today. The seam this adds is *how* a transport is obtained, so a
/// later firmware can hand in a `PoolConnector` backed by an
/// in-process pipe instead of a TCP socket without touching this
/// function or `run_stratum_client`.
///
/// # Errors
///
/// [`SessionError::Connect`] when `connector.connect()` fails.
/// [`SessionError::Session`] when the connected [`run_stratum_client`]
/// does; see its docs for the exact conditions.
pub async fn run_stratum_session<P, C, const N: usize>(
    connector: &mut P,
    client: &mut C,
    asic: PoolHandle<'_, N>,
    state: &mut StratumState,
    ntime_roll_period_secs: Option<u32>,
) -> Result<(), SessionError<P::Error>>
where
    P: PoolConnector,
    C: MiningClient,
{
    let mut transport = connector.connect().await.map_err(SessionError::Connect)?;
    run_stratum_client(client, &mut transport, asic, state, ntime_roll_period_secs)
        .await
        .map_err(SessionError::Session)
}
