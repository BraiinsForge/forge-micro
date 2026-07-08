//! Shared stratum client orchestration task.
//!
//! Hoisted out of the seven board `main.rs` files, where this ~95-line session
//! loop was duplicated byte-for-byte. The only per-board variance was the SV2
//! noise-handshake RNG: every board now seeds a `ChaCha20Rng` at boot from its
//! hardware entropy and passes it in, so the task, the secp context, the
//! shared `AsicSession`, and the `Sv2BoardRng` adapter are all shared here.

use embassy_futures::select::{Either, select};
use embassy_time::Timer;
use rand_chacha::ChaCha20Rng;
use rand_chacha::rand_core::{RngCore, SeedableRng};
use static_cell::{ConstStaticCell, StaticCell};
use stratum_client::{
    AsicSession, NTIME_ROLL_PERIOD_SECS, PoolConnector, RECONNECT_DELAY, SOCKET_TIMEOUT,
    STRATUM_TCP_BUFFER_SIZE, SessionError, StratumState, record_reconnect_attempt,
};
use ubos_logging::{Debug2Format, info, warn};

/// Shared ASIC session: the bridge between the stratum work source and the
/// BM1362 serial worker. One instance per binary; referenced by both the
/// stratum client task and the per-board mining serial task.
pub static ASIC_SESSION: AsicSession = AsicSession::new();

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

/// SV2 noise-handshake RNG. Wraps the boot-seeded `ChaCha20` CSPRNG so the
/// stratum-v2 client draws ephemeral keys and nonces from one shared entropy
/// stream, independent of the session CSPRNG.
struct Sv2BoardRng<'a>(&'a mut ChaCha20Rng);

impl ii_sv2::security::Rng for Sv2BoardRng<'_> {
    fn fill_bytes(&mut self, dest: &mut [u8]) {
        RngCore::fill_bytes(self.0, dest);
    }
}

/// Boot-time storage for the SV2 `ChaCha20` PRNG.
static SV2_PRNG: StaticCell<ChaCha20Rng> = StaticCell::new();

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
fn log_session_error(error: &SessionError<ubos_net::net::StratumConnectError>) {
    match error {
        SessionError::Connect(error) => {
            warn!("stratum-client: connect failed: {:?}", Debug2Format(error));
        }
        SessionError::Session(error) => {
            warn!("stratum-client: session error {:?}", Debug2Format(error));
        }
    }
}

/// Seed the SV2 `ChaCha20` PRNG once at boot, returning the `'static` handle each
/// board hands to [`run_stratum_session`].
///
/// `seed` must come from the board's hardware entropy source. Every board funds
/// its SV2 handshake from a hardware-seeded `ChaCha20` CSPRNG that outlives boot
/// as a `'static` rather than being reconstructed per reconnect, so the stream
/// stays continuous across reconnects and no board has to keep its hardware RNG
/// peripheral live past seeding.
#[must_use]
pub fn seed_sv2_prng(seed: [u8; 32]) -> &'static mut ChaCha20Rng {
    SV2_PRNG.init(ChaCha20Rng::from_seed(seed))
}

/// Connects to the configured pool and runs stratum sessions, reconnecting on
/// error and restarting on pool-config changes.
///
/// This is a plain `async fn`, not an `#[embassy_executor::task]`: esp32s3
/// pins a different `embassy-executor` version than the ARM boards, so the
/// task shell that owns the pool must stay in each board binary and compile
/// against that board's executor. Each board wraps this body in a one-line
/// task. The shared session loop lives here only once.
// The only panics are `.expect()` on compile-time-sized buffer invariants
// (BUG-class, not caller-facing), so a `# Panics` doc section would mislead.
#[allow(clippy::too_many_lines, clippy::missing_panics_doc)]
pub async fn run_stratum_session(
    stack: embassy_net::Stack<'static>,
    sv2_prng: &'static mut ChaCha20Rng,
) -> ! {
    static TCP_RX: ConstStaticCell<[u8; STRATUM_TCP_BUFFER_SIZE]> =
        ConstStaticCell::new([0; STRATUM_TCP_BUFFER_SIZE]);
    static TCP_TX: ConstStaticCell<[u8; STRATUM_TCP_BUFFER_SIZE]> =
        ConstStaticCell::new([0; STRATUM_TCP_BUFFER_SIZE]);
    let rx_buffer = TCP_RX.take();
    let tx_buffer = TCP_TX.take();

    // SV2 secp context — initialised once even if the first pool entry
    // is Sv1, so a runtime protocol switch needs no late allocation.
    let secp_buffer = SECP_CTX_BUF.take();
    let secp_ctx = SECP_CTX.init(
        ii_sv2::security::CryptoCtx::preallocated(secp_buffer)
            .expect("BUG: SECP_CTX_SLOTS too small for AllPreallocated"),
    );

    let config_handle = ubos_config::handle();
    let restart = config_handle.stratum_restart();

    loop {
        let snapshot = config_handle.current();
        let Some(entry) = snapshot.pool.first() else {
            warn!("stratum-client: pool list empty, awaiting config update");
            restart.wait().await;
            continue;
        };

        let one_session = async {
            record_reconnect_attempt();
            let mut connector = TcpConnector {
                stack,
                host: entry.host.as_str(),
                port: entry.port,
                rx_buffer: &mut *rx_buffer,
                tx_buffer: &mut *tx_buffer,
            };
            let mut state = StratumState::new();

            match &entry.protocol {
                ubos_config::ProtocolKind::Sv1 { password } => {
                    let sv1 = stratum_v1::Sv1Config::new(
                        entry.user.as_str(),
                        password.as_ref().map_or("", heapless::String::as_str),
                    )
                    .expect("BUG: PoolEntry user/password match Sv1Config buffer sizes");
                    let mut client = stratum_v1::StratumV1Client::new(&sv1);
                    if let Err(error) = stratum_client::run_stratum_session(
                        &mut connector,
                        &mut client,
                        &ASIC_SESSION,
                        &mut state,
                        Some(NTIME_ROLL_PERIOD_SECS),
                    )
                    .await
                    {
                        log_session_error(&error);
                    }
                }
                ubos_config::ProtocolKind::Sv2 { pool_pubkey } => {
                    let sv2 = stratum_v2::Sv2Config::new(entry.user.as_str(), *pool_pubkey)
                        .expect("BUG: PoolEntry.user matches Sv2Config buffer size");
                    let mut client = stratum_v2::StratumV2Client::new(
                        &sv2,
                        secp_ctx,
                        Sv2BoardRng(&mut *sv2_prng),
                    );
                    if let Err(error) = stratum_client::run_stratum_session(
                        &mut connector,
                        &mut client,
                        &ASIC_SESSION,
                        &mut state,
                        Some(NTIME_ROLL_PERIOD_SECS),
                    )
                    .await
                    {
                        log_session_error(&error);
                    }
                }
            }

            Timer::after(RECONNECT_DELAY).await;
        };
        if matches!(
            select(one_session, restart.wait()).await,
            Either::Second(())
        ) {
            info!("stratum-client: pool config changed, reconnecting");
        }
    }
}
