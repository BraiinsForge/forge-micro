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

//! Shared stratum client orchestration task.
//!
//! Hoisted out of the board `main.rs` files, where this session
//! loop was duplicated byte-for-byte. One instance of
//! [`run_stratum_session`] runs per pool slot: each maintains its own
//! connection to its configured endpoint, publishes work into its slot
//! of the shared [`BoardAsicSession`], and drains its slot's
//! solutions. Which slot actually feeds the chip is decided on the
//! ASIC side, by `ubos-mining`'s dispatcher work scheduler; the
//! sessions here are policy-free.
//!
//! Boot-time singletons (the SV2 secp context, the SV2 entropy seed)
//! are initialised once via [`init_stratum`] and fanned out to the
//! per-slot tasks as a [`StratumShared`] value.

use crate::AsicSession;
use embassy_futures::select::{Either, select};
use embassy_time::Timer;
use rand_chacha::ChaCha20Rng;
use rand_chacha::rand_core::{RngCore, SeedableRng};
use static_cell::{ConstStaticCell, StaticCell};
use stratum_client::{
    NtimeRoll, PoolConnector, RECONNECT_DELAY, SESSION_REJECTED_DELAY, SOCKET_TIMEOUT,
    STRATUM_TCP_BUFFER_SIZE, SessionError, StratumState,
};
use ubos_logging::{Debug2Format, info, warn};

/// Config pool-list capacity, re-exported as the default value for
/// each board's `POOL_SLOTS` const. A board may choose fewer session
/// slots than the config persists (RAM-constrained boards). A board
/// that installs a fee pool chooses `ubos_config::POOL_SLOTS`, one
/// more, and no board chooses beyond that: a session task for a slot
/// past it parks forever.
pub use ubos_config::pool::MAX_POOLS;

// Preallocated buffer for the SV2 `secp256k1::AllPreallocated` context.
// `CryptoCtx::preallocated` needs a `[AlignedType]` slice at least
// `secp256k1_context_preallocated_size(All)` slots long. Modern
// libsecp256k1 (secp256k1-sys 0.11) holds the ecmult and ecmult-gen
// tables in static `.rodata`, so the runtime context is tiny: that size
// is 13 `AlignedType` slots (208 B), measured against the compiled C
// library. 64 slots (1 KiB) keep a ~5x margin, and
// `CryptoCtx::preallocated` returns `NotEnoughMemory` (caught by the
// boot `expect`) if ever undersized.
const SECP_CTX_SLOTS: usize = 64;

static SECP_CTX_BUF: ConstStaticCell<[ii_sv2::security::AlignedType; SECP_CTX_SLOTS]> =
    ConstStaticCell::new([ii_sv2::security::AlignedType::ZERO; SECP_CTX_SLOTS]);

static SECP_CTX: StaticCell<
    ii_sv2::security::CryptoCtx<ii_sv2::security::secp256k1::AllPreallocated<'static>>,
> = StaticCell::new();

/// SV2 noise-handshake RNG. Wraps a per-slot `ChaCha20` CSPRNG so each
/// stratum-v2 client draws ephemeral keys and nonces from its own
/// entropy stream, independent of the other slots and of the session
/// CSPRNG.
struct Sv2BoardRng<'a>(&'a mut ChaCha20Rng);

impl ii_sv2::security::Rng for Sv2BoardRng<'_> {
    fn fill_bytes(&mut self, dest: &mut [u8]) {
        RngCore::fill_bytes(self.0, dest);
    }
}

/// Boot-time stratum singletons, fanned out by value to every per-slot
/// session task. `Copy`: holds a shared reference to the secp context,
/// the 32-byte SV2 entropy seed, and the board's choice of who rolls
/// ntime.
#[derive(Clone, Copy)]
pub struct StratumShared {
    secp_ctx:
        &'static ii_sv2::security::CryptoCtx<ii_sv2::security::secp256k1::AllPreallocated<'static>>,
    sv2_seed: [u8; 32],
    ntime_roll: NtimeRoll,
}

/// Initialise the boot-time stratum singletons once, returning the
/// [`StratumShared`] value each board hands to every
/// [`run_stratum_session`] task.
///
/// `chip_rolls_ntime` is the chip descriptor's `rolls_ntime`: a chip
/// that searches ntime itself gets each job published once, every
/// other chip gets the host's ntime roll.
///
/// `sv2_seed` must come from the board's hardware entropy source. Each
/// slot's task derives its own `ChaCha20` stream from the one seed
/// (`set_stream(slot)`), so the per-slot noise-handshake RNGs are
/// mutually independent, and each stream outlives boot inside its task
/// rather than being reconstructed per reconnect — continuous across
/// reconnects, and no board keeps its hardware RNG peripheral live
/// past seeding.
///
/// # Panics
///
/// Panics if called twice (the underlying cells are single-take), or
/// if `SECP_CTX_SLOTS` undersizes the secp context (BUG-class,
/// compile-time-sized invariant).
#[must_use]
pub fn init_stratum(sv2_seed: [u8; 32], chip_rolls_ntime: bool) -> StratumShared {
    let ntime_roll: NtimeRoll = chip_rolls_ntime.into();
    let secp_buffer = SECP_CTX_BUF.take();
    let secp_ctx: &'static _ = SECP_CTX.init(
        ii_sv2::security::CryptoCtx::preallocated(secp_buffer)
            .expect("BUG: SECP_CTX_SLOTS too small for AllPreallocated"),
    );
    StratumShared {
        secp_ctx,
        sv2_seed,
        ntime_roll,
    }
}

/// TCP transport source for one pool entry. Owns the stack handle and
/// host/port, borrows the task's static socket buffers; rebuilt per
/// config-snapshot iteration alongside the protocol client.
struct TcpConnector<'a> {
    stack: embassy_net::Stack<'static>,
    host: &'a str,
    port: u16,
    rx_buffer: &'a mut [u8],
    tx_buffer: &'a mut [u8],
}

impl PoolConnector for TcpConnector<'_> {
    type Conn<'c>
        = embassy_net::tcp::TcpSocket<'c>
    where
        Self: 'c;
    type Error = ubos_net::net::StratumConnectError;

    async fn connect(&mut self) -> Result<Self::Conn<'_>, Self::Error> {
        ubos_net::net::connect_stratum(
            self.stack,
            self.host,
            self.port,
            SOCKET_TIMEOUT,
            self.rx_buffer,
            self.tx_buffer,
        )
        .await
    }
}

/// Log a session failure, preserving the distinct connect-failed and
/// session-error lines the pre-connector code emitted.
fn log_session_error(slot: usize, error: &SessionError<ubos_net::net::StratumConnectError>) {
    match error {
        SessionError::Connect(error) => {
            warn!(
                "stratum-client: pool {} connect failed: {:?}",
                slot,
                Debug2Format(error)
            );
        }
        SessionError::Session(error) => {
            warn!(
                "stratum-client: pool {} session error {:?}",
                slot,
                Debug2Format(error)
            );
        }
    }
}

/// Maintains one connection to the pool that `slot` resolves to
/// through [`ubos_config::Handle::pool_slot`], reconnecting on error
/// and restarting on pool-config changes. A slot with no entry parks
/// until the next config update — which is permanent for a slot past
/// `ubos_config::POOL_SLOTS`, so boards size `N` at or below it.
///
/// `N` is the board's session-slot count, chosen by a `POOL_SLOTS`
/// const in each board's `main.rs` and carried here by the generic:
/// RAM-constrained boards run fewer parallel sessions than the config
/// persists.
///
/// This is a plain `async fn`, not an `#[embassy_executor::task]`:
/// esp32s3 pins a different `embassy-executor` version than the ARM
/// boards, so the task shell that owns the pool must stay in each
/// board binary and compile against that board's executor. Each board
/// wraps this body in a one-line task spawned once per slot
/// (`pool_size = POOL_SLOTS`). The shared session loop lives here
/// only once.
// The only panics are `.expect()` on compile-time-sized buffer invariants
// (BUG-class, not caller-facing), so a `# Panics` doc section would mislead.
#[allow(clippy::too_many_lines, clippy::missing_panics_doc)]
pub async fn run_stratum_session<const N: usize>(
    stack: embassy_net::Stack<'static>,
    slot: usize,
    session: &'static AsicSession<N>,
    shared: StratumShared,
) -> ! {
    // This session's socket buffers. Locals held across `.await` live
    // in the task's statically allocated future, so each task instance
    // owns its pair with no shared-static bookkeeping.
    let mut rx_buffer = [0u8; STRATUM_TCP_BUFFER_SIZE];
    let mut tx_buffer = [0u8; STRATUM_TCP_BUFFER_SIZE];

    // This slot's ChaCha20 stream over the shared boot seed: mutually
    // independent between slots, continuous across reconnects within
    // the task.
    let mut sv2_prng = ChaCha20Rng::from_seed(shared.sv2_seed);
    sv2_prng.set_stream(slot as u64);

    let handle = session.pool_handle(slot);
    // Lives across reconnects so assignment ids stay unique per slot.
    let mut state = StratumState::new(slot);

    let config_handle = ubos_config::handle();
    let mut config_generation = config_handle.subscribe_generation();

    loop {
        // Teardown boundary: whether the last iteration ended in a
        // session error, a config change, or never ran, the slot's
        // work cell must not offer a previous session's job to the
        // dispatcher while no session backs it.
        handle.clear_work();
        // The alive bit must also drop here: a config change cancels
        // `one_session` mid-await, so `run_stratum_client` never
        // reaches its own on-return clear, and a slot parked on a
        // removed entry would otherwise stay "Alive" forever.
        handle.set_alive(false);

        let Some(entry) = config_handle.pool_slot(slot) else {
            // An empty primary slot means no mining at all — worth a
            // warning. Higher slots are backup entries; absent ones
            // are the normal single-pool configuration.
            if slot == 0 {
                warn!("stratum-client: pool list empty, awaiting config update");
            }
            config_generation.changed().await;
            continue;
        };

        let one_session = async {
            handle.record_reconnect_attempt();
            let mut connector = TcpConnector {
                stack,
                host: entry.host.as_str(),
                port: entry.port,
                rx_buffer: &mut rx_buffer[..],
                tx_buffer: &mut tx_buffer[..],
            };
            state.restart();

            let result = match &entry.protocol {
                ubos_config::ProtocolKind::Sv1 { password } => {
                    let sv1 = stratum_v1::Sv1Config::new(
                        entry.user.as_str(),
                        password.as_ref().map_or("", heapless::String::as_str),
                    )
                    .expect("BUG: PoolEntry user/password match Sv1Config buffer sizes");
                    let mut client = stratum_v1::StratumV1Client::new(&sv1);
                    stratum_client::run_stratum_session(
                        &mut connector,
                        &mut client,
                        handle,
                        &mut state,
                        shared.ntime_roll.period_secs(),
                    )
                    .await
                }
                ubos_config::ProtocolKind::Sv2 { pool_pubkey } => {
                    let sv2 = stratum_v2::Sv2Config::new(entry.user.as_str(), *pool_pubkey)
                        .expect("BUG: PoolEntry.user matches Sv2Config buffer size");
                    let mut client = stratum_v2::StratumV2Client::new(
                        &sv2,
                        shared.secp_ctx,
                        Sv2BoardRng(&mut sv2_prng),
                    );
                    stratum_client::run_stratum_session(
                        &mut connector,
                        &mut client,
                        handle,
                        &mut state,
                        shared.ntime_roll.period_secs(),
                    )
                    .await
                }
            };

            let delay = match &result {
                Err(error) => {
                    log_session_error(slot, error);
                    if error.is_rejection() {
                        SESSION_REJECTED_DELAY
                    } else {
                        RECONNECT_DELAY
                    }
                }
                Ok(()) => RECONNECT_DELAY,
            };
            Timer::after(delay).await;
        };
        if matches!(
            select(one_session, config_generation.changed()).await,
            Either::Second(_)
        ) {
            info!("stratum-client: pool {} config changed, reconnecting", slot);
        }
    }
}
