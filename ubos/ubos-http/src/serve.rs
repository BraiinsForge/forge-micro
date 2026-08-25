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

//! Edge-http accept loop wrapper for firmware HTTP servers.
//!
//! `Service` is the application-level seam for the edge-http migration
//! path. Implement `Service` in a handler crate, pass it to `bind_tcp`
//! to get a `'static` acceptor, then spawn one `serve_worker` task per
//! pool slot using the `http_worker_task!` macro. Per-worker state lives
//! in each task's own `TaskStorage` (linker-checked), so a pool that
//! cannot fit in flash or RAM fails the *build* rather than crashing the
//! board at runtime.

use core::net::{IpAddr, Ipv4Addr, SocketAddr};

use edge_http::io::Error as HttpError;
use edge_http::io::server::Connection;
use edge_nal::{Close, TcpAccept as _, TcpBind as _, TcpShutdown as _};
use edge_nal_embassy::{Tcp, TcpAccept};
use embassy_time::{Duration, with_timeout};
use embedded_io_async::{Read, Write};
use ubos_logging::{Debug2Format, warn};

use edge_nal::TcpSplit;

/// Maximum number of HTTP headers parsed per request.
///
/// Sixteen headers cover all headers sent by Prometheus scrapers and
/// browsers without inflating the per-connection stack frame.
pub(crate) const MAX_HEADERS: usize = 16;

/// Body of the 500 response written when a handler returns an error.
const INTERNAL_ERROR_BODY: &str = "INTERNAL ERROR";

/// Application-level handler for a single edge-http connection.
///
/// The trait is the migration-target seam between the transport crate
/// and handler crates such as `ubos-api`. Implement `Service` and hand
/// the value to `serve_worker` (typically via the `http_worker_task!`
/// macro); the accept loop calls `handle` once per accepted TCP
/// connection after the HTTP request headers have been parsed by
/// edge-http.
///
/// `peer_ip` carries the remote address extracted from the accepted
/// socket so that handlers can enforce IP allow-lists without depending
/// on embassy-net types directly.
#[allow(async_fn_in_trait)]
pub trait Service {
    /// Handle one edge-http connection.
    ///
    /// `peer_ip` is the remote IP address of the accepted TCP
    /// connection. `connection` is the edge-http state machine for the
    /// request-response cycle; the handler reads request headers via
    /// `connection.headers()` and writes the response via
    /// `connection.initiate_response` followed by writes to the
    /// connection writer.
    async fn handle<TcpSocket, const N: usize>(
        &self,
        peer_ip: IpAddr,
        connection: &mut Connection<'_, TcpSocket, N>,
    ) -> Result<(), HttpError<TcpSocket::Error>>
    where
        TcpSocket: Read + Write + TcpSplit;
}

/// Bind a `'static` TCP acceptor on `port`.
///
/// Calls `TcpBind::bind` on the provided `'static` [`Tcp`] and returns the
/// resulting `TcpAccept<'static>`. The acceptor is `Copy`, so callers can
/// hand one copy to each worker task spawned with `http_worker_task!`.
///
/// The caller must store the `Tcp` in a `StaticCell` and pass the
/// `&'static Tcp<'static>` here; only a `'static` borrow produces a
/// `TcpAccept<'static>`.
///
/// # Panics
///
/// Panics if `TcpBind::bind` returns an error. The current
/// `edge-nal-embassy` implementation of `bind` is infallible and
/// only stores the address; a panic here would indicate a library
/// contract violation.
pub async fn bind_tcp(tcp: &'static Tcp<'static>, port: u16) -> TcpAccept<'static> {
    let bind_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), port);

    #[expect(
        clippy::expect_used,
        reason = "TcpBind::bind on edge-nal-embassy is infallible"
    )]
    // edge-nal-embassy TcpBind::bind() is documented infallible;
    // a failure here is a library contract violation.
    tcp.bind(bind_addr).await.expect("BUG: TCP bind failed")
}

/// One accept-loop worker, suitable for use as an embassy task body.
///
/// Loops forever: accepts one TCP connection, then serves requests on it
/// until the handler asks to close, the peer goes idle past the idle
/// timeout, or a protocol error occurs, then loops back to accept. A
/// response that sends `Connection: close` ends the connection after one
/// request; a response that omits it keeps the connection alive for the
/// next request. Only the metrics handler closes, so a browser reuses one
/// connection across the page assets and the polls that follow them.
///
/// The `acceptor` is `Copy` so each spawned task holds its own copy and
/// all workers compete on the same bound port. `parse_buffer` is the
/// worker's private edge-http header parse area; `service` is the
/// `'static` handler shared across all workers.
///
/// Use `http_worker_task!` to generate a concrete pool of
/// `#[embassy_executor::task]` wrappers that call this function.
///
/// The exchange is driven here rather than via edge-http's
/// `handle_connection` so the worker can bound each of its two waits
/// separately. `socket_timeout` is installed via `set_timeout` and bounds
/// progress: it reclaims a connection that stalls mid-exchange while
/// still allowing an actively-streaming upload to run as long as bytes
/// keep arriving. `idle_timeout` bounds the wait for the request headers,
/// so a connection that opens and sends nothing, and a kept-alive
/// connection the peer leaves idle, both free their worker on the shorter
/// value. One timeout cannot do both jobs: the value that frees an
/// abandoned browser socket quickly is shorter than the value an upload
/// needs to survive a flash-write freeze.
pub async fn serve_worker<S>(
    acceptor: TcpAccept<'static>,
    parse_buffer: &'static mut [u8],
    service: &'static S,
    socket_timeout: Duration,
    idle_timeout: Duration,
) -> !
where
    S: Service,
{
    loop {
        let (peer_addr, mut socket): (SocketAddr, _) = match acceptor.accept().await {
            Ok(pair) => pair,
            Err(error) => {
                warn!("http accept error: {:?}", Debug2Format(&error));
                continue;
            }
        };

        // Install the per-listener progress timeout so smoltcp reaps a
        // stalled peer while letting an OTA upload stream as long as it
        // keeps making progress.
        socket.set_timeout(Some(socket_timeout));

        // Serve requests on this connection until one asks to close. The
        // metrics handler always sends `Connection: close`, so
        // `needs_close` is true after its first response; the merged web
        // UI and REST listener omits it, so its connection is reused for
        // the next request.
        loop {
            match serve_one(
                &mut *parse_buffer,
                &mut socket,
                service,
                peer_addr.ip(),
                idle_timeout,
            )
            .await
            {
                // Keep-alive: the response did not ask to close, so loop and
                // serve the next request on the same socket.
                Ok(false) => {}
                // The handler asked to close. A graceful FIN lets the client
                // read the final response cleanly; bound it so a vanished
                // peer cannot block the worker.
                Ok(true) => {
                    let _ = with_timeout(socket_timeout, socket.close(Close::Both)).await;
                    break;
                }
                // A protocol error, or a connection the peer left idle past
                // the idle timeout, is reset: a graceful close would itself
                // block draining toward an unresponsive peer.
                Err(Ended) => {
                    let _ = socket.abort().await;
                    break;
                }
            }
        }
    }
}

/// The connection is over and the worker must reset the socket.
///
/// One type covers every way a request can end the connection: the idle
/// bound expires, the request does not parse, or the response cannot be
/// written. The worker takes the same path for all three, so it needs no
/// detail beyond the fact.
struct Ended;

/// Serve one request on `socket` and report whether the response asked to
/// close the connection.
///
/// This is `edge_http::io::server::handle_request` unrolled, so that the
/// wait for the request headers can carry `idle_timeout` on its own while
/// the exchange that follows keeps only the socket timeout the caller
/// installed. `handle_request` runs both steps in one call and therefore
/// cannot bound the first alone.
///
/// # Errors
///
/// Returns `Ended` when the idle bound expires, when the request does not
/// parse, or when the response cannot be written to the socket.
async fn serve_one<S, T>(
    parse_buffer: &mut [u8],
    socket: T,
    service: &S,
    peer_ip: IpAddr,
    idle_timeout: Duration,
) -> Result<bool, Ended>
where
    S: Service,
    T: Read + Write + TcpSplit,
{
    // The idle bound expired, or the request did not parse. The
    // connection is over either way and the caller resets the socket.
    let Ok(Ok(mut connection)) = with_timeout(
        idle_timeout,
        Connection::<_, MAX_HEADERS>::new(parse_buffer, socket),
    )
    .await
    else {
        return Err(Ended);
    };

    // A handler error still owes the client a response, so write the 500
    // rather than dropping the socket with nothing on it.
    match service.handle(peer_ip, &mut connection).await {
        Ok(()) => connection.complete().await,
        Err(_) => connection.complete_err(INTERNAL_ERROR_BODY).await,
    }
    .map_err(|_| Ended)?;

    Ok(connection.needs_close())
}

/// Split a `'static mut` byte slice into `N` equal `'static mut` chunks.
///
/// Each returned chunk covers a non-overlapping `chunk_size`-byte region
/// of the original buffer, so the chunks can be safely handed to `N`
/// independent worker tasks.
///
/// # Panics
///
/// Panics if `buffer.len() != chunk_size * N`.
///
/// # Safety
///
/// Safe: the original `buffer` is `'static mut`, so the base pointer
/// is valid for the entire program. Each chunk covers a disjoint range
/// with no aliases — the caller passes each chunk to exactly one task.
pub fn split_work_buffer<const N: usize>(
    buffer: &'static mut [u8],
    chunk_size: usize,
) -> [&'static mut [u8]; N] {
    // Buffer must be exactly chunk_size * N bytes so each worker gets
    // an equal-sized, non-overlapping parse area.
    assert!(
        buffer.len() == chunk_size * N,
        "BUG: buffer.len() must equal chunk_size * N"
    );

    let base = buffer.as_mut_ptr();
    // Each chunk covers a distinct, non-overlapping address range within
    // the 'static buffer. No two indices produce the same pointer.
    core::array::from_fn(|index| unsafe {
        core::slice::from_raw_parts_mut(base.add(index * chunk_size), chunk_size)
    })
}

/// Generate a pool of `#[embassy_executor::task]` HTTP worker tasks.
///
/// # Syntax
///
/// ```text
/// http_worker_task!(task_name, ServiceType, POOL_CONST)
/// ```
///
/// Expands to an `#[embassy_executor::task(pool_size = POOL_CONST)]`
/// async function named `task_name` that calls `serve_worker` with the
/// given service type. `POOL_CONST` may be any `const` expression,
/// including a path such as `config::MACHINE.control_board.http_pool`.
///
/// # Example
///
/// ```ignore
/// // In the board crate:
/// ubos_http::http_worker_task!(
///     metrics_worker_task,
///     ubos_metrics::metrics::PrometheusMetricsService,
///     METRICS_POOL
/// );
///
/// // Then in async main:
/// for chunk in work_chunks {
///     spawner.spawn(unwrap!(metrics_worker_task(
///         acceptor, chunk, service,
///         ubos_metrics::consts::Metrics::SOCKET_TIMEOUT,
///         ubos_metrics::consts::Metrics::IDLE_TIMEOUT,
///     )));
/// }
/// ```
#[macro_export]
macro_rules! http_worker_task {
    ($name:ident, $service_type:ty, $pool_const:expr) => {
        #[embassy_executor::task(pool_size = $pool_const)]
        async fn $name(
            acceptor: $crate::TcpAccept<'static>,
            parse_buffer: &'static mut [u8],
            service: &'static $service_type,
            socket_timeout: $crate::Duration,
            idle_timeout: $crate::Duration,
        ) -> ! {
            $crate::serve::serve_worker(
                acceptor,
                parse_buffer,
                service,
                socket_timeout,
                idle_timeout,
            )
            .await
        }
    };
}

/// Declare one pool static, in CCM on a board that has it.
///
/// The two definitions are selected by **this** crate's `ccmram-bss`
/// feature, which is why they are two `#[cfg]` definitions rather than
/// one `#[cfg_attr]` inside the body. A `#[cfg_attr]` written in the
/// body would read the feature set of the board crate that expands
/// [`serve_http_pool!`], and the decision does not belong there.
///
/// The buffers qualify for CCM on two counts. The ethernet DMA reaches
/// only the driver's own packet queue, and smoltcp copies between that
/// queue and these buffers, so nothing here is a DMA target. And both
/// statics are all-zero at rest, which is what `.ccmram.bss` requires:
/// `TcpBuffers` is a pool of `Cell<bool>` flags that start `false` over
/// a `MaybeUninit` payload, and the parse area is a zeroed array.
#[cfg(feature = "ccmram-bss")]
#[macro_export]
macro_rules! http_pool_static {
    ($name:ident: $ty:ty = $init:expr) => {
        #[unsafe(link_section = ".ccmram.bss")]
        static $name: $ty = $init;
    };
}

/// Declare one pool static in main SRAM. See the `ccmram-bss` variant.
#[cfg(not(feature = "ccmram-bss"))]
#[macro_export]
macro_rules! http_pool_static {
    ($name:ident: $ty:ty = $init:expr) => {
        static $name: $ty = $init;
    };
}

/// Bind one HTTP listener and spawn its pool of `serve_worker` tasks.
///
/// # Syntax
///
/// ```text
/// serve_http_pool!(spawner, stack, worker_task, service, POOL_CONST, consts_path)
/// ```
///
/// Expands to a block that allocates the pool's `TcpBuffers`, `Tcp`, and
/// split work buffer in block-scoped statics, binds `consts_path::PORT`,
/// and spawns `POOL_CONST` instances of `worker_task` (declared with
/// [`http_worker_task!`]). `service` is a `&'static` reference to the
/// listener's [`Service`] handler; `consts_path` is a type path exposing
/// the `TX_BUFFER_SIZE`, `RX_BUFFER_SIZE`, `REQUEST_BUFFER_SIZE`,
/// `SOCKET_TIMEOUT`, `IDLE_TIMEOUT`, and `PORT` associated constants,
/// such as `ubos_metrics::consts::Metrics`.
///
/// Each invocation expands to its own block, so the fixed static names do
/// not collide when several listeners are set up in one function. The
/// pool footprint is sized by `POOL_CONST` const expressions, so an
/// oversized pool fails the link rather than overflowing at boot.
///
/// # Example
///
/// ```ignore
/// ubos_http::http_worker_task!(
///     metrics_worker_task,
///     ubos_metrics::metrics::PrometheusMetricsService,
///     METRICS_POOL
/// );
///
/// // In async main:
/// ubos_http::serve_http_pool!(
///     spawner, stack, metrics_worker_task, &METRICS_SERVICE,
///     METRICS_POOL, ubos_metrics::consts::Metrics
/// );
/// ```
///
/// Boards on embassy-executor 0.9.x (esp-rtos) must prefix the arguments
/// with `must_spawn;`. On 0.9.x the task constructor returns a
/// `SpawnToken` directly and `Spawner::spawn` returns a `Result`, so the
/// pool is spawned with `must_spawn`, matching how those boards spawn
/// every other task. The default form is for 0.10.x, where the
/// constructor returns `Result<SpawnToken, _>` and `spawn` is infallible.
#[macro_export]
macro_rules! serve_http_pool {
    ($spawner:expr, $stack:expr, $worker:ident, $service:expr, $pool:expr, $consts:path $(,)?) => {
        $crate::serve_http_pool!(@setup spawn; $spawner, $stack, $worker, $service, $pool, $consts)
    };
    (must_spawn; $spawner:expr, $stack:expr, $worker:ident, $service:expr, $pool:expr, $consts:path $(,)?) => {
        $crate::serve_http_pool!(@setup must_spawn; $spawner, $stack, $worker, $service, $pool, $consts)
    };
    (@setup $mode:ident; $spawner:expr, $stack:expr, $worker:ident, $service:expr, $pool:expr, $consts:path) => {{
        $crate::http_pool_static!(TCP_BUFFERS: $crate::ConstStaticCell<
            $crate::TcpBuffers<
                { $pool },
                { <$consts>::TX_BUFFER_SIZE },
                { <$consts>::RX_BUFFER_SIZE },
            >,
        > = $crate::ConstStaticCell::new($crate::TcpBuffers::new()));
        static TCP_NAL: $crate::StaticCell<$crate::Tcp<'static>> =
            $crate::StaticCell::new();
        $crate::http_pool_static!(WORK: $crate::ConstStaticCell<
            [u8; <$consts>::REQUEST_BUFFER_SIZE * $pool],
        > = $crate::ConstStaticCell::new([0; <$consts>::REQUEST_BUFFER_SIZE * $pool]));

        let tcp: &'static $crate::Tcp<'static> =
            TCP_NAL.init($crate::Tcp::new($stack, TCP_BUFFERS.take()));
        let acceptor = $crate::bind_tcp(tcp, <$consts>::PORT).await;
        let buffers =
            $crate::split_work_buffer::<{ $pool }>(WORK.take(), <$consts>::REQUEST_BUFFER_SIZE);
        for buffer in buffers {
            $crate::serve_http_pool!(
                @spawn $mode; $spawner, $worker, acceptor, buffer, $service,
                <$consts>::SOCKET_TIMEOUT, <$consts>::IDLE_TIMEOUT
            );
        }
    }};
    (@spawn spawn; $spawner:expr, $worker:ident, $acceptor:expr, $buffer:expr, $service:expr, $timeout:expr, $idle:expr) => {
        $spawner.spawn(::defmt::unwrap!($worker(
            $acceptor, $buffer, $service, $timeout, $idle
        )))
    };
    (@spawn must_spawn; $spawner:expr, $worker:ident, $acceptor:expr, $buffer:expr, $service:expr, $timeout:expr, $idle:expr) => {
        $spawner.must_spawn($worker($acceptor, $buffer, $service, $timeout, $idle))
    };
}
