# edge-nal-embassy: TcpAccept::accept() returns local endpoint instead of remote endpoint

## Problem

`TcpAccept::accept()` in `edge-nal-embassy` returns the *local* (server-bound) socket
address as the `SocketAddr` in its `Ok` tuple instead of the remote peer's address.
The `edge_nal::TcpAccept` trait contract says the returned address is "the socket address
of the remote peer" (doc comment on the trait method). Callers that use this value for
IP-based allow-list filtering silently check the server's own address on every incoming
connection, making the check always pass and the filter ineffective.

## Reproduction

Upstream source: `edge-nal-embassy/src/tcp.rs`, `TcpAccept::accept()` implementation,
commit `1f084b41944c35b233cbe17accfbc09a56d5eaf0`.

```rust
// line 108 in upstream
let local_endpoint = unwrap!(socket.socket.local_endpoint());
Ok((to_net_socket(local_endpoint), socket))
```

The `embassy_net::tcp::TcpSocket::local_endpoint()` method returns the address the
server socket is *bound* to, not the connecting peer's address. Every call to `accept()`
therefore returns the same bound address regardless of which client connected.

## Proposed fix

```diff
--- a/edge-nal-embassy/src/tcp.rs
+++ b/edge-nal-embassy/src/tcp.rs
@@ -105,9 +105,12 @@
             .accept(to_emb_bind_socket(self.local).ok_or(TcpError::UnsupportedProto)?)
             .await?;
 
-        let local_endpoint = unwrap!(socket.socket.local_endpoint());
+        // Return the peer's address so callers can inspect the remote IP.
+        // The original code called local_endpoint(), which returns the bound
+        // server address and defeats any IP-based filtering at the handler.
+        let remote_endpoint = unwrap!(socket.socket.remote_endpoint());
 
-        Ok((to_net_socket(local_endpoint), socket))
+        Ok((to_net_socket(remote_endpoint), socket))
     }
 }
```

`embassy_net::tcp::TcpSocket::remote_endpoint()` returns `Option<IpEndpoint>` just like
`local_endpoint()`, so the `unwrap!` wrapper and `to_net_socket` call are unchanged.
After `accept()` succeeds the socket is in ESTABLISHED state and `remote_endpoint()` is
guaranteed to return `Some`.

## Risk

Purely a bug fix against a documented contract; no API surface changes. Existing callers
that ignored the returned address are unaffected. Callers that used the address for
allow-listing were silently broken before and will now work correctly.

## Suggested PR title

`edge-nal-embassy: Fix accept() returning local instead of remote endpoint`

## Suggested PR body

`TcpAccept::accept()` returns `(SocketAddr, Socket)` where the address is documented as
the remote peer's address. The current implementation passes `local_endpoint()` to
`to_net_socket`, which always produces the server's own bound address.

**Impact:** any downstream handler that uses the returned `SocketAddr` for IP-based
filtering checks the server's own address on every request; the filter is effectively
disabled.

**Fix:** call `remote_endpoint()` instead. After a successful `accept()` the socket is
in ESTABLISHED state so `remote_endpoint()` is always `Some`.

One-line change, no API break.
