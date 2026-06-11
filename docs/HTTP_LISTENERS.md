# HTTP listener concurrency

Status: design note for a future branch. Scope: the `ubos-http` accept
loop and its consumers (`ubos-webui` :80, `ubos-api` :8080,
`ubos-metrics` :8081).

The firmware HTTP servers currently handle **one connection at a time**
per listener. This note records why, the two problems it causes, the
target capacity, and what an implementation has to change. An interim
UX mitigation (option C below) already shipped; the real fix is deferred
to its own branch because it is extensive.

## Current design

`ubos_http::serve` (`ubos/ubos-http/src/serve.rs`) runs a sequential
accept loop:

```
loop {
    let (peer, socket) = acceptor.accept().await;
    with_timeout(Net::SOCKET_TIMEOUT, handle_connection(socket, work_buffer, ...)).await;
}
```

It is hard-wired to a single connection:

- The socket pool is `TcpBuffers<1, TX, RX>` — one socket buffer.
- A single `work_buffer` (the request-parse buffer) is reused across the
  loop. `serve` asserts `TCP_POOL <= 1` precisely because that buffer
  would be raced by concurrent connections.

So a connection is accepted, fully served (request → response → close),
and only then is the next one accepted.

Buffer sizes (`ubos/ubos-core/src/consts.rs`):

| Listener | RX | TX | request-parse | per-connection |
|----------|----|----|---------------|----------------|
| :80 `Http` | 2 KiB | 2 KiB | 2 KiB | **6 KiB** |
| :8080 `Api` | 2 KiB | 2 KiB | 2 KiB | **6 KiB** |
| :8081 `Metrics` | 512 B | 1 KiB | 512 B | **2 KiB** |

## Problem 1 — concurrent clients serialize

Two browsers on the dashboard do not fail, but their requests are
serialized: while one connection is being handled, the other waits in
the TCP backlog. Requests are short and HTTP closes after each, so they
interleave and mostly work — but under load the dashboard's 2 s poll
"loses a race against another fetch" (already noted in `index.js`), which
is why the SPA carries retry/backoff. More clients means more races.

## Problem 2 — ~30 s unreachable after a live network apply

Measured on bmm-pico: after `POST /api/system/network`, the inbound
listeners (:80, :8080) are unreachable for a **constant ~32 s**, while
the stack stays healthy the whole time (the stratum client keeps
submitting shares and receiving jobs across the apply; no reboot, no link
drop, the change applies and persists correctly).

Cause: applying the config severs the inbound TCP connection at the IP
layer with no FIN/RST exchanged — the peer simply vanishes. When the
handler ends, smoltcp does a **graceful close**: it sends a FIN and waits
for an ACK that never comes, retransmits, and only frees the socket after
the close timeout (~30 s). With `TCP_POOL = 1` that lone socket is the
only one, so no new connection — including the operator's re-login — can
be accepted until it frees.

The single-socket limit is the common root of both problems: a dead
socket is not just *a* socket, it is *the* socket.

## Can the buffers be shared?

Question that comes up: can multiple clients share these buffers?

- **Socket RX/TX buffers: no, not while connections are concurrently
  live.** Each open connection holds in-flight bytes in its own RX/TX
  buffer; sharing would corrupt streams. `TcpBuffers<N>` is a *reuse
  pool* across time, not concurrent sharing — N simultaneous connections
  genuinely need N buffer pairs.
- **Request-parse (`work_buffer`): partially.** It is only needed during
  the brief header-parse phase, not for the whole connection. A small
  pool of K parse buffers could be borrowed during parse and released, so
  K < N buffers serve N connections if they do not all parse at the same
  instant. This trades RAM for borrow/release bookkeeping and a
  contention fallback. Simplest correct design is still one parse buffer
  per concurrent connection.
- **Right-sizing is independent of count.** Per-listener buffers can be
  trimmed (e.g. :80 mostly serves small gzipped shells) to claw back RAM
  regardless of how many connections each allows.

## Target capacity

- :8080 (API) — **4** clients
- :80 (web UI shell + login) — **2** clients
- :8081 (metrics) — **2** clients

RAM delta over today (one connection each), at current buffer sizes:

| Listener | now | target | added |
|----------|-----|--------|-------|
| :8080 | 6 KiB | 4 × 6 = 24 KiB | +18 KiB |
| :80 | 6 KiB | 2 × 6 = 12 KiB | +6 KiB |
| :8081 | 2 KiB | 2 × 2 = 4 KiB | +2 KiB |
| **total** | | | **+26 KiB** |

## Per-board constraint

The buffer sizes and the socket count are shared constants across all
firmware. The RAM budget is not shared:

- **bmm-pico / rp2350 (RP2350):** ample SRAM; +26 KiB is comfortable.
- **rp2040 (256 KiB):** ran with ~240 bytes of free SRAM (see the
  `Metrics::RX_BUFFER_SIZE` comment) — cannot absorb even one extra
  socket without trimming elsewhere.
- **stm32f407 (128 KiB):** the tightest; same problem.

So the connection count must be **per-board**, not a global bump: higher
on rp2350/bmm-pico, left at 1 on rp2040 and stm32f407. A const-generic or
per-board const threaded into `serve` and the `TcpBuffers`/`work_buffer`
statics is the natural mechanism.

## What an implementation must change

1. **Make `serve` concurrent.** Drop the shared `work_buffer` and the
   `TCP_POOL <= 1` assert. Give each in-flight connection its own
   parse buffer (or a borrowed-pool slot), and service connections in
   parallel — either spawn a task per accepted connection or run a small
   fixed pool of handler futures. `handle_connection` already takes the
   socket and a buffer per call, so the change is in the loop and the
   static buffer wiring.
2. **Parameterize the connection count per board** and size the
   `TcpBuffers<N>` / parse-buffer statics from it.
3. **Check `StackResources<16>`.** Each listener socket draws from the
   board's embassy-net socket pool, shared with stratum, NTP, DHCP, and
   mDNS. Confirm the added sockets fit under 16 (or raise it, which also
   costs RAM).
4. **Abort instead of graceful-close on a severed connection.** Even with
   a spare socket, a dead one should not idle for ~30 s. Either set a
   short `set_timeout` on accepted sockets so smoltcp resets them quickly,
   or restructure so the loop owns the socket and can `abort()` (RST) it
   when a connection breaks. This is the same fix that would address
   Problem 2 on its own.

## Interim mitigation (shipped)

Option C: the post-apply "Network configuration changed" notice tells the
operator the board can take up to ~30 s to become reachable, so the delay
reads as expected rather than as a failure. The single-connection limit
and the ~30 s stall remain until the work above lands.
