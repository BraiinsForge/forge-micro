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

//! Edge-http accept loop for the firmware HTTP listeners. A handler
//! crate implements [`Service`]; the board binds a port with [`bind_tcp`]
//! and spawns one [`serve_worker`] per pool slot through
//! `http_worker_task!`. Per-worker state lives in each task's own
//! storage, so a pool that does not fit fails the link, not the board.

use core::net::{IpAddr, Ipv4Addr, SocketAddr};

use edge_http::io::Error as HttpError;
use edge_http::io::server::Connection;
use edge_nal::{Close, TcpAccept as _, TcpBind as _, TcpShutdown as _};
use edge_nal_embassy::{Tcp, TcpAccept};
use embassy_time::{Duration, with_timeout};
use embedded_io_async::{Read, Write};
use ubos_logging::{Debug2Format, warn};

use edge_nal::TcpSplit;

use crate::request::RequestParts;

/// Headers parsed per request: headroom over a browser's sixteen or so
/// plus `Authorization`.
pub(crate) const MAX_HEADERS: usize = 32;

/// Written raw when a request carries more than [`MAX_HEADERS`], since
/// the parse failed and no `Connection` exists to respond through.
const HEADER_OVERFLOW_RESPONSE: &[u8] =
    b"HTTP/1.1 431 Request Header Fields Too Large\r\nConnection: close\r\nContent-Length: 0\r\n\r\n";

const INTERNAL_ERROR_BODY: &str = "INTERNAL ERROR";

/// Handler for one request, called by the worker after edge-http parsed
/// the headers into a [`RequestParts`].
#[allow(async_fn_in_trait)]
pub trait Service {
    /// The handler reads any body and writes the response through
    /// `connection`. `peer_ip` is for allow-lists.
    async fn handle<TcpSocket, const N: usize>(
        &self,
        peer_ip: IpAddr,
        parts: &RequestParts,
        connection: &mut Connection<'_, TcpSocket, N>,
    ) -> Result<(), HttpError<TcpSocket::Error>>
    where
        TcpSocket: Read + Write + TcpSplit;
}

/// Bind a `'static` TCP acceptor on `port`. The acceptor is `Copy`, one
/// per worker.
///
/// # Panics
///
/// If `TcpBind::bind` fails, which `edge-nal-embassy` documents it
/// cannot.
pub async fn bind_tcp(tcp: &'static Tcp<'static>, port: u16) -> TcpAccept<'static> {
    let bind_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), port);

    #[expect(
        clippy::expect_used,
        reason = "TcpBind::bind on edge-nal-embassy is infallible"
    )]
    tcp.bind(bind_addr).await.expect("BUG: TCP bind failed")
}

/// One accept-loop worker. A connection stays alive across requests
/// until a response sends `Connection: close`, the peer idles past
/// `idle_timeout`, or the request fails to parse.
///
/// The exchange is driven here instead of by edge-http's
/// `handle_connection` so the two waits carry separate bounds:
/// `socket_timeout` bounds progress, so an upload survives a flash-write
/// freeze as long as bytes keep arriving, and `idle_timeout` bounds the
/// wait for headers, so an abandoned browser socket frees its worker
/// fast. One value cannot do both.
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

        socket.set_timeout(Some(socket_timeout));

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
                Ok(false) => {}
                // A graceful FIN lets the client read the final response;
                // bounded so a vanished peer cannot hold the worker.
                Ok(true) => {
                    let _ = with_timeout(socket_timeout, socket.close(Close::Both)).await;
                    break;
                }
                // A live client owed an answer rather than a reset; the
                // progress timeout bounds the write.
                Err(Ended::TooManyHeaders) => {
                    if socket.write_all(HEADER_OVERFLOW_RESPONSE).await.is_ok() {
                        let _ = socket.flush().await;
                    }
                    let _ = with_timeout(socket_timeout, socket.close(Close::Both)).await;
                    break;
                }
                // Reset, since a graceful close would block draining toward
                // an unresponsive peer.
                Err(Ended::Reset) => {
                    let _ = socket.abort().await;
                    break;
                }
            }
        }
    }
}

/// How the worker must end a finished connection. The header overflow
/// is the one failure whose peer is a live client owed an answer.
enum Ended {
    Reset,
    /// Answer with a 431 before closing.
    TooManyHeaders,
}

/// Serve one request and report whether the response asked to close.
/// This is edge-http's `handle_request` unrolled, so the wait for the
/// headers can carry `idle_timeout` on its own.
///
/// # Errors
///
/// `Ended` when the idle bound expires, the request does not parse, or
/// the response cannot be written.
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
    let mut connection = match with_timeout(
        idle_timeout,
        Connection::<_, MAX_HEADERS>::new(parse_buffer, socket),
    )
    .await
    {
        Ok(Ok(connection)) => connection,
        Ok(Err(HttpError::TooManyHeaders)) => return Err(Ended::TooManyHeaders),
        _ => return Err(Ended::Reset),
    };

    // The extraction cannot fail straight after `new`, but the client is
    // owed a response either way, so it gets the 500.
    let Ok(parts) = crate::request::extract_request_parts(&mut connection) else {
        connection
            .complete_err(INTERNAL_ERROR_BODY)
            .await
            .map_err(|_| Ended::Reset)?;
        return Ok(connection.needs_close());
    };

    match service.handle(peer_ip, &parts, &mut connection).await {
        Ok(()) => connection.complete().await,
        Err(_) => connection.complete_err(INTERNAL_ERROR_BODY).await,
    }
    .map_err(|_| Ended::Reset)?;

    Ok(connection.needs_close())
}

/// Split a `'static mut` byte slice into `N` disjoint `'static mut`
/// chunks, one per worker.
///
/// # Panics
///
/// If `buffer.len() != chunk_size * N`.
pub fn split_work_buffer<const N: usize>(
    buffer: &'static mut [u8],
    chunk_size: usize,
) -> [&'static mut [u8]; N] {
    assert!(
        buffer.len() == chunk_size * N,
        "BUG: buffer.len() must equal chunk_size * N"
    );

    let base = buffer.as_mut_ptr();
    // SAFETY: the ranges are disjoint and the buffer is `'static`.
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
/// including a path such as
/// `config::MACHINE.control_board.http_pool_sizes.http`.
///
/// # Example
///
/// ```ignore
/// // In the board crate:
/// ubos_http::http_worker_task!(
///     metrics_worker_task,
///     ubos_metrics::metrics::PrometheusMetricsService<64, 4096>,
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

/// `serve_http_pool!(spawner, stack, worker_task, service, POOL_CONST,
/// consts_path)` allocates a listener's buffers in block-scoped statics,
/// binds `consts_path::PORT` and spawns `POOL_CONST` workers.
/// `consts_path` is a type with the buffer sizes, the two timeouts and
/// `PORT`, such as `ubos_metrics::consts::Metrics`. A board on
/// embassy-executor 0.9.x prefixes the arguments with `must_spawn;`,
/// because its task constructor returns the token directly.
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
