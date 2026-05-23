//! Edge-http accept loop wrapper for firmware HTTP servers.
//!
//! `Service` is the application-level seam for the edge-http migration
//! path. Implement `Service` in a handler crate, pass it to `serve`,
//! and the accept loop takes care of binding, accepting, and driving
//! the edge-http state machine for each connection.
//!
//! The peer IP is extracted from the accepted `SocketAddr` and forwarded
//! to `Service::handle` so handlers can enforce IP allow-lists without
//! touching any embassy-net types directly.

use core::net::{IpAddr, Ipv4Addr, SocketAddr};

use edge_http::io::server::{handle_connection, Connection};
use edge_http::io::Error as HttpError;
use edge_nal::{TcpAccept as _, TcpBind as _};
use edge_nal_embassy::{Tcp, TcpBuffers};
use embassy_net::Stack;
use embassy_time::with_timeout;
use embedded_io_async::{Read, Write};
use ubos_core::consts::Net;
use ubos_logging::{warn, Debug2Format};

use edge_nal::TcpSplit;

/// Maximum number of HTTP headers parsed per request.
///
/// Sixteen headers cover all headers sent by Prometheus scrapers and
/// browsers without inflating the per-connection stack frame.
const MAX_HEADERS: usize = 16;

/// Logging task identifier passed to `edge_http::io::server::handle_connection`.
///
/// The accept loop serves one connection at a time, so a single fixed
/// identifier is sufficient; edge-http only uses it for log line
/// tagging.
const TASK_ID: usize = 0;

/// Application-level handler for a single edge-http connection.
///
/// The trait is the migration-target seam between the transport crate
/// and handler crates such as `ubos-api`. Implement `Service` and pass
/// the value to `serve`; the accept loop calls `handle` once per
/// accepted TCP connection after the HTTP request headers have been
/// parsed by edge-http.
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

/// Bind on `port` and serve `service` for every accepted TCP connection.
///
/// One TCP socket is consumed per accepted connection; the socket is
/// returned to `tcp_buffers` when the connection is dropped. The
/// `work_buffer` is reused across connections as the edge-http header
/// parse area. The function never returns; on accept error it logs a
/// warning and continues.
///
/// # Panics
///
/// Panics if `TcpBind::bind` returns an error. The current
/// `edge-nal-embassy` implementation of `bind` is infallible and
/// only stores the address; a panic here would indicate a library
/// contract violation.
///
/// # Arguments
///
/// - `stack` — the embassy-net stack.
/// - `port` — the TCP port to listen on.
/// - `service` — the handler called for each accepted connection.
/// - `tcp_buffers` — pool of TX/RX socket buffers. Pool depth controls
///   how many simultaneous connections the server can handle.
/// - `work_buffer` — edge-http header parse buffer, reused per
///   connection. Sized to `ubos_core::consts::Http::REQUEST_BUFFER_SIZE`
///   or larger by the caller.
pub async fn serve<S, const TCP_POOL: usize, const TX: usize, const RX: usize>(
    stack: Stack<'static>,
    port: u16,
    service: S,
    tcp_buffers: &'static TcpBuffers<TCP_POOL, TX, RX>,
    work_buffer: &'static mut [u8],
) -> !
where
    S: Service,
{
    // Single work_buffer is reused across the accept loop; concurrent
    // connections would race on it. The assert enforces the invariant at
    // runtime on first call, which is effectively compile-time because
    // TCP_POOL is a const generic and the panic is immediate on startup.
    assert!(
        TCP_POOL <= 1,
        "BUG: ubos-http serve currently shares work_buffer across the accept loop; TCP_POOL > 1 would race",
    );

    let tcp = Tcp::new(stack, tcp_buffers);

    let bind_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), port);

    // bind() on Tcp is infallible in the current edge-nal-embassy
    // implementation — it only stores the address. The `expect` is
    // present to satisfy the Result return type and would only fire if
    // the underlying implementation changes to fail at bind time.
    #[expect(
        clippy::expect_used,
        reason = "TcpBind::bind on edge-nal-embassy is infallible"
    )]
    let acceptor = tcp
        .bind(bind_addr)
        .await
        // edge-nal-embassy TcpBind::bind() is documented infallible;
        // a failure here would be a library contract violation.
        .expect("BUG: TCP bind failed");

    loop {
        let (peer_addr, socket): (SocketAddr, _) = match acceptor.accept().await {
            Ok(pair) => pair,
            Err(error) => {
                warn!("http accept error: {:?}", Debug2Format(&error));
                continue;
            }
        };

        let peer_ip = peer_addr.ip();

        let handler = AdaptedHandler {
            service: &service,
            peer_ip,
        };

        // Cap per-connection lifetime so a LAN-attached client that
        // opens TCP but never sends a request cannot hold the single
        // TCP_POOL=1 accept slot indefinitely and starve subsequent
        // requests. Matches the equivalent guard in ubos-metrics's
        // accept loop.
        if with_timeout(
            Net::SOCKET_TIMEOUT,
            handle_connection::<_, _, MAX_HEADERS>(socket, work_buffer, None, TASK_ID, handler),
        )
        .await
        .is_err()
        {
            warn!("http connection timed out");
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
