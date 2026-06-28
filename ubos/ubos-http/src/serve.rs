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
use edge_http::io::server::{Connection, handle_request};
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
const MAX_HEADERS: usize = 16;

/// Logging task identifier passed to `edge_http::io::server::handle_request`.
///
/// All concurrent accept-loop workers share this identifier; edge-http
/// uses it only for log line tagging, so a single fixed value is
/// sufficient.
const TASK_ID: usize = 0;

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
/// until the handler asks to close, the peer goes idle past the socket
/// timeout, or a protocol error occurs, then loops back to accept. A
/// response that sends `Connection: close` ends the connection after one
/// request; a response that omits it keeps the connection alive for the
/// next request. The REST and metrics handlers always close, so only the
/// web UI reuses a connection across the burst of asset requests a browser
/// issues per page load.
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
/// `handle_connection` so the worker can install the caller-provided
/// `socket_timeout` via `set_timeout`. That single inactivity timeout
/// reclaims a connection that opens but never sends a request, one that
/// stalls mid-exchange, and a kept-alive connection the peer leaves idle,
/// while still allowing an actively-streaming upload to run as long as it
/// keeps making progress. Listeners tune it per port: short on the web UI
/// so a kept-alive browser socket frees its worker quickly, longer on the
/// REST API so an OTA upload tolerates a slow link.
pub async fn serve_worker<S>(
    acceptor: TcpAccept<'static>,
    parse_buffer: &'static mut [u8],
    service: &'static S,
    socket_timeout: Duration,
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

        // Install the per-listener timeout so smoltcp reaps a stalled peer
        // while letting an OTA upload stream as long as it keeps making
        // progress. On a kept-alive connection it also bounds the wait for
        // the next request, so an idle web-UI socket frees its worker after
        // the web UI's short timeout rather than the base ten seconds.
        socket.set_timeout(Some(socket_timeout));

        let handler = AdaptedHandler::new(service, peer_addr.ip());

        // Serve requests on this connection until one asks to close. The
        // REST and metrics handlers always send `Connection: close`, so
        // `needs_close` is true after their first response; the web UI omits
        // it, so its connection is reused for the next request.
        loop {
            match handle_request::<_, _, MAX_HEADERS>(
                &mut *parse_buffer,
                &mut socket,
                TASK_ID,
                &handler,
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
                // A protocol error, or a kept-alive connection the peer left
                // idle past the timeout, is reset: a graceful close would
                // itself block draining toward an unresponsive peer.
                Err(_) => {
                    let _ = socket.abort().await;
                    break;
                }
            }
        }
    }
}

/// Adapts `Service` to the `edge_http::io::server::Handler` trait.
///
/// One `AdaptedHandler` is constructed per accepted connection. It
/// captures the peer IP extracted from the accept call and forwards it
/// to `Service::handle` alongside the edge-http `Connection`.
struct AdaptedHandler<'svc, S> {
    service: &'svc S,
    peer_ip: IpAddr,
}

impl<'svc, S> AdaptedHandler<'svc, S> {
    /// Wrap `service` for one accepted connection, capturing the peer IP
    /// to forward to `Service::handle`.
    fn new(service: &'svc S, peer_ip: IpAddr) -> Self {
        Self { service, peer_ip }
    }
}

impl<S> edge_http::io::server::Handler for AdaptedHandler<'_, S>
where
    S: Service,
{
    type Error<E>
        = HttpError<E>
    where
        E: core::fmt::Debug;

    async fn handle<T, const N: usize>(
        &self,
        _task_id: impl core::fmt::Display + Copy,
        connection: &mut Connection<'_, T, N>,
    ) -> Result<(), Self::Error<T::Error>>
    where
        T: Read + Write + TcpSplit,
    {
        self.service.handle(self.peer_ip, connection).await
    }
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
/// including a path such as `config::Board::WEBUI_HTTP_POOL`.
///
/// # Example
///
/// ```ignore
/// // In the board crate:
/// ubos_http::http_worker_task!(webui_worker_task, ubos_webui::DashboardService, config::Board::WEBUI_HTTP_POOL);
///
/// // Then in async main:
/// for chunk in work_chunks {
///     spawner.spawn(unwrap!(webui_worker_task(
///         acceptor, chunk, service, ubos_core::consts::Http::SOCKET_TIMEOUT,
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
        ) -> ! {
            $crate::serve::serve_worker(acceptor, parse_buffer, service, socket_timeout).await
        }
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
/// `SOCKET_TIMEOUT`, and `PORT` associated constants, such as
/// `ubos_core::consts::Http`.
///
/// Each invocation expands to its own block, so the fixed static names do
/// not collide when several listeners are set up in one function. The
/// pool footprint is sized by `POOL_CONST` const expressions, so an
/// oversized pool fails the link rather than overflowing at boot.
///
/// # Example
///
/// ```ignore
/// ubos_http::http_worker_task!(webui_worker_task, ubos_webui::DashboardService, config::Board::WEBUI_HTTP_POOL);
///
/// // In async main:
/// ubos_http::serve_http_pool!(
///     spawner, stack, webui_worker_task, &WEBUI_SERVICE,
///     config::Board::WEBUI_HTTP_POOL, ubos_core::consts::Http
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
        static TCP_BUFFERS: $crate::ConstStaticCell<
            $crate::TcpBuffers<
                { $pool },
                { <$consts>::TX_BUFFER_SIZE },
                { <$consts>::RX_BUFFER_SIZE },
            >,
        > = $crate::ConstStaticCell::new($crate::TcpBuffers::new());
        static TCP_NAL: $crate::StaticCell<$crate::Tcp<'static>> =
            $crate::StaticCell::new();
        static WORK: $crate::ConstStaticCell<
            [u8; <$consts>::REQUEST_BUFFER_SIZE * $pool],
        > = $crate::ConstStaticCell::new([0; <$consts>::REQUEST_BUFFER_SIZE * $pool]);

        let tcp: &'static $crate::Tcp<'static> =
            TCP_NAL.init($crate::Tcp::new($stack, TCP_BUFFERS.take()));
        let acceptor = $crate::bind_tcp(tcp, <$consts>::PORT).await;
        let buffers =
            $crate::split_work_buffer::<{ $pool }>(WORK.take(), <$consts>::REQUEST_BUFFER_SIZE);
        for buffer in buffers {
            $crate::serve_http_pool!(
                @spawn $mode; $spawner, $worker, acceptor, buffer, $service,
                <$consts>::SOCKET_TIMEOUT
            );
        }
    }};
    (@spawn spawn; $spawner:expr, $worker:ident, $acceptor:expr, $buffer:expr, $service:expr, $timeout:expr) => {
        $spawner.spawn(::defmt::unwrap!($worker($acceptor, $buffer, $service, $timeout)))
    };
    (@spawn must_spawn; $spawner:expr, $worker:ident, $acceptor:expr, $buffer:expr, $service:expr, $timeout:expr) => {
        $spawner.must_spawn($worker($acceptor, $buffer, $service, $timeout))
    };
}
