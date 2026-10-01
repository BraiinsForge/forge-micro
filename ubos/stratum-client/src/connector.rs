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

//! `PoolConnector` — the transport-acquisition seam for a stratum
//! session.
//!
//! `run_stratum_client` already treats the transport as an opaque
//! `Read + Write` object; this trait exists one layer up so that
//! *obtaining* that transport, whether a TCP socket or an in-process
//! pipe, is pluggable independent of the protocol dispatch and
//! work-generation loop. Reconnect policy and pool-config dispatch
//! stay with the callers — see [`run_stratum_session`].

use embedded_io_async::{Read, Write};
use mining_protocol::{Error, MiningClient};

use crate::asic_session::PoolHandle;
use crate::extension::SessionExtension;
use crate::run::run_stratum_client;
use crate::state::StratumState;

#[cfg(test)]
#[path = "tests/connector.rs"]
mod tests;

/// Yields one connected transport per call. Implementations own the
/// address/buffer plumbing, whether TCP via embassy-net or an
/// in-process pipe.
///
/// `Conn` is a generic associated type borrowing from the connector —
/// the shape `embedded_nal_async::TcpConnect` established. A TCP
/// implementation hands its socket buffers to `TcpSocket<'a>`, so the
/// connection must be allowed to borrow from `&mut self`; a
/// self-contained implementation, such as an in-process pipe owning
/// its storage, simply ignores the lifetime.
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

impl<E> SessionError<E> {
    /// True when the session ended in a permanent refusal — the pool
    /// rejected the session, or the authenticator declined the pool's
    /// certificate — rather than the transport or protocol failing.
    /// Callers retry these on [`crate::SESSION_REJECTED_DELAY`]
    /// instead of the transport cadence.
    pub fn is_rejection(&self) -> bool {
        matches!(
            self,
            Self::Session(Error::SessionRejected(_) | Error::AuthFailed)
        )
    }
}

/// Obtain one connection from `connector`, then run
/// [`run_stratum_client`] over it.
///
/// This function does not own the outer reconnect loop: it makes
/// exactly one connect attempt and runs exactly one session. The
/// caller keeps looping, watching config-restart signals, and
/// rebuilding protocol clients and connectors per iteration.
///
/// # Errors
///
/// [`SessionError::Connect`] when `connector.connect()` fails.
/// [`SessionError::Session`] when the connected [`run_stratum_client`]
/// does; see its docs for the exact conditions.
pub async fn run_stratum_session<P, C, const N: usize, const CHAINS: usize>(
    connector: &mut P,
    client: &mut C,
    asic: PoolHandle<'_, N, CHAINS>,
    state: &mut StratumState,
    ntime_roll_period_secs: Option<u32>,
    extension: &mut dyn SessionExtension,
) -> Result<(), SessionError<P::Error>>
where
    P: PoolConnector,
    C: MiningClient,
{
    let mut transport = connector.connect().await.map_err(SessionError::Connect)?;
    run_stratum_client(
        client,
        &mut transport,
        asic,
        state,
        ntime_roll_period_secs,
        extension,
    )
    .await
    .map_err(SessionError::Session)
}
