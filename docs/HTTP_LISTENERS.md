# HTTP listener concurrency

Status: implemented. Scope: the `ubos-http` accept loop and its consumers
(`ubos-webui` :80, `ubos-api` :8080, `ubos-metrics` :8081).

The firmware HTTP servers serve several connections at once through a
fixed pool of accept-loop workers, sized per board. This note records the
shipped design, how to reason about the viable client count, the measured
numbers on hardware, why the pool cannot simply be cranked up, and how to
reproduce the measurement.

## Current design

`ubos_http::serve_worker` (`ubos/ubos-http/src/serve.rs`) is the
per-connection accept loop. Each board spawns a pool of independent
worker tasks per listener via the `http_worker_task!` macro — one embassy
task per worker:

- `bind_tcp` binds the listener once and returns a `'static` acceptor that
  every worker shares (it is `Copy`).
- `split_work_buffer` hands each worker a non-overlapping `'static` slice
  of the parse buffer.
- each worker owns one socket from a `TcpBuffers<POOL, TX, RX>` pool,
  accepts a connection, serves it, and loops back to accept.

The depth is a per-board constant (`WEBUI_HTTP_POOL` / `API_HTTP_POOL` /
`METRICS_HTTP_POOL` in each `config.rs`) that sizes the `TcpBuffers`, the
parse buffer, and the worker task pool. Because each worker is its own task with its own static
future, the whole per-listener footprint is fixed and linker-checked; it
does not depend on how deeply any single future is polled. An earlier
design folded the whole worker pool into one `join_array` future, which
grew with the pool and overflowed the stack at boot for large pools — the
per-task structure removes that failure mode (see below).

Two properties matter for capacity:

- **Every response sends `Connection: close`.** There is no HTTP
  keep-alive on any listener (confirmed on hardware: the authenticated
  `200` to `GET /api/info` closes just like the `401`). So a worker is not
  pinned to a client across polls — each poll is a fresh, short-lived
  connection (~20 ms on the LAN), and the worker returns to `accept`
  immediately after closing.
- **Teardown is bounded.** A worker waits at most `IDLE_TIMEOUT` (5 s) for a
  request to start arriving, and at most `REQUEST_TIMEOUT`
  (`Net::SOCKET_TIMEOUT`, 10 s) for one exchange to complete. A completed
  exchange that asked to close gets a bounded graceful close; every other
  exit (idle past `IDLE_TIMEOUT`, a vanished peer, a timed-out request)
  aborts with a reset. This is what
  removed the earlier ~30 s unreachable window after a live network apply,
  where a single graceful-closing socket blocked all new connections.

Buffer sizes (`ubos/ubos-core/src/consts.rs`):

| Listener | RX | TX | request-parse | per-connection |
|----------|----|----|---------------|----------------|
| :80 `Http` | 2 KiB | 2 KiB | 2 KiB | **6 KiB** |
| :8080 `Api` | 2 KiB | 2 KiB | 2 KiB | **6 KiB** |
| :8081 `Metrics` | 512 B | 1 KiB | 512 B | **2 KiB** |

Shipped pool depths are uniform per board and uniform across all three
listeners: `WEBUI_HTTP_POOL`, `API_HTTP_POOL`, and `METRICS_HTTP_POOL` are
each `5` on the ~512 KiB boards (bmm-pico, rp2350, esp32s3) and `1` on the
RAM-constrained boards (rp2040, stm32f407, stm32f439). Each board takes the
largest depth its RAM links: the tight boards do not link at `5` per
listener (see "bounded at link time" below), so they drop to `1` rather
than to an intermediate depth. The web UI gets the same depth as the API
because a browser fans out several parallel connections to fetch the page
assets on load; the REST API depth sets the live-watcher capacity (below).
The metrics listener uses the same pool so a Prometheus scrape draws its
own worker instead of contending for a single accept slot; a dedicated
ingest task, not the accept loop, now drains the push channel into the
shared metric map.

## Capacity model

Because there is no keep-alive, the right metric is **peak simultaneous
in-flight connections**, not the number of viewers. For `B` browsers each
polling one endpoint every `P` seconds with request duration `D`:

- **Pool size** must cover the peak in-flight, which ranges from
  `B × D/P` when clients are desynchronized up to `B` when they poll in
  lockstep. A connection that cannot get a socket is refused, and the
  client retries on its next poll.
- **`StackResources<N>`** must cover the continuously held sockets plus the
  TIME_WAIT backlog. The server is the active closer, so it holds the
  TIME_WAIT; the close rate is about `B / P` per second, each lingering
  until smoltcp reaps it.

The dashboard polls `/api/info` every 2 s (`ubos-webui/src/index.js`), so
`P = 2 s` and `D ≈ 20 ms`. The web UI assets are fetched once at page load
and not again, so in steady state a viewer exercises only the API
listener.

## Single-connection transfer is window-limited by the TX buffer

The concurrency model above governs how many connections run at once; a
separate limit governs how fast any one connection moves bytes. For the
dashboard payload this is the dominant cost, and it is **not** the TCP
initial congestion window the `ubos-webui` size-budget comments invoke.

A connection can have at most `TX_BUFFER_SIZE` unacknowledged bytes in
flight, because the server must retain unacked data for retransmission and
that data lives in the socket TX buffer. With `Http::TX_BUFFER_SIZE =
2048`, at most ~2 KiB is ever outstanding, so a single connection delivers
about **2 KiB per round trip** regardless of how much the client will
accept. The ~16 KiB gzipped dashboard therefore takes roughly **8 round
trips**, not the one or two a 14 KiB initcwnd would imply.

The client receive window is never the bottleneck: Windows and macOS
default to ~64 KiB before window scaling, and Linux autotunes from ~64 KiB
into the MiB range (`tcp_rmem` default 128 KiB, max 6 MiB on a current
laptop). All are far above the server's 2 KiB.

Consequences:

- **The gzipped-size budget is essentially academic on a LAN.** At a
  sub-millisecond LAN RTT, 8 round trips is a few milliseconds. Whether the
  page is 14 KiB or 17 KiB changes the round-trip count by one or two, not
  the user-visible load time. The "first paint must fit one 14 KiB
  initcwnd" rationale in `ubos-webui`'s size test and `build.rs` does not
  describe this firmware; the real wall is 2 KiB/RTT.
- **Do not split assets to chase the budget.** Moving the dashboard JS into
  a second request would add a full TCP handshake — every response is
  `Connection: close`, so there is no warm connection to reuse — to save a
  byte count that the TX buffer already makes irrelevant.
- **The lever for load time, if it ever matters, is `TX_BUFFER_SIZE`, not
  gz size.** Raising it (RAM permitting) widens the per-connection window;
  shrinking the payload does not change the 2 KiB/RTT ceiling. Note this
  trades against the concurrency footprint, since TX buffer is per worker.

## Measured on bmm-pico (RP2350)

Using `http-load-test` (see below), with clients desynchronized
by ±0.8 s of jitter:

| `API_HTTP_POOL` | clean viewers (≥99%) | above the pool |
|-----------------|----------------------|----------------|
| 2 | 2 | degrades gracefully to ~85% |
| 5 | 5 | degrades gracefully to ~91% at 8 |

The clean concurrency ceiling tracks the pool size one-to-one. Above it,
desynchronized clients degrade gracefully (a fraction of polls are refused
and retried, ~5 ms median latency throughout) rather than collapsing, and
the board stays up — an overloaded listener drops connections, it does not
crash. Only perfectly synchronized clients hit the hard
`success ≈ pool / clients` worst case.

**Shipped target: 5 concurrent watchers** on the 512 KiB boards, verified
on bmm-pico: boots, serves 5 clients at 100 %, and survives an 8-client
overload with only dropped polls.

## Pool size is bounded at link time

Each worker is its own task with its own static future, so the entire
per-listener footprint — `TcpBuffers`, the parse buffer, and the worker
task pool — is static and **linker-checked**. An over-sized pool is a
build error, not a runtime fault, and `cargo build --release` is what
discovers each board's ceiling. Measured with the web UI and API pools at
`5` each (ten worker futures), before the metrics listener joined the pool
design:

- **rp2040** overflows `.bss` by ~50 KiB (264 KiB SRAM, no CCM).
- **stm32f407** overflows main `.bss` by ~64 KiB *and* `.ccmram.bss` by
  ~8 KiB — too small at 128 KiB main + 64 KiB CCM even with the buffers in
  CCM.
- **stm32f439** overflows `.ccmram.bss` by ~7 KiB and the stack by a few
  hundred bytes: ten worker futures plus the 5/5 buffers do not fit in its
  combined RAM.

The dominant cost is the per-listener worker `TaskStorage` futures, which
live in main SRAM regardless of where the buffers sit. That is why the
three tight boards ship at `1` per listener and only the ~512 KiB boards
(bmm-pico, rp2350, esp32s3) link at `5`; with the metrics listener on the
same pool those boards now carry fifteen worker futures across the three
listeners and still link. On stm32f407 and stm32f439 the TCP and parse
buffers are placed in CCM (`.ccmram.bss`) so the depth-1 buffers stay off
the tight main SRAM.

This is a deliberate change from an earlier design that folded the
whole worker pool into one `join_array` future. That future grew with the
pool, and **constructing** it on the first poll consumed call stack
proportional to the pool — which is not linker-checked (cortex-m-rt parks
the stack at the top of RAM and lets it grow down toward `.bss`, with no
guard). Large pools silently overran the stack and hard-faulted at boot
(`8` crash-looped, `10` was dead on arrival) with meaningless backtraces.
Moving each worker to its own task moves the whole footprint into the
statically verified column: if it links, it fits.

The general rule: **data** is statically allocated and linker-checked; the
**call stack** is not. The per-task structure keeps each worker's state in
its own static future and each `poll` stack small and independent of the
pool count, so the only thing that scales with the pool is linker-checked
static data.

## Optional: stack-overflow guard

A stack guard is not required to bound the pool footprint (that is now
linker-checked), but it would still catch a regression in handler code that
deepens a single `poll` past the available stack, turning a silent
corruption into an immediate, labelled fault:

- **`flip-link`** relocates the stack below `.bss`/`.data`, so an overflow
  runs off the bottom into the boundary and faults at once.
- The **Cortex-M33 `MSPLIM` stack-limit register** (present on the RP2350)
  faults the instant the stack pointer crosses `_stack_limit`.

## Further headroom

- **Raise a pool where the board links it.** With the footprint
  linker-checked, bumping `API_HTTP_POOL` is safe to try: it either links
  (and fits) or fails the build. Validate with a release build and the load
  test.
- **Share one pool across :80 and :8080.** The web UI (bursty at page load)
  and the API (steady low-rate polling) are largely used at different
  times, so two separately sized pools reserve RAM for peaks that do not
  coincide. A shared pool drawn from one `TcpBuffers` would serve more total
  clients with less reserved RAM; the constraint is that `edge-nal-embassy`
  binds an acceptor per port, so the two acceptors would have to draw from
  one shared buffer pool.

## Can the buffers be shared?

- **Socket RX/TX buffers: no, not while connections are concurrently
  live.** Each open connection holds in-flight bytes in its own RX/TX
  buffer. `TcpBuffers<N>` is a reuse pool across time, not concurrent
  sharing — `N` simultaneous connections need `N` buffer pairs.
- **Request-parse buffer: partially.** It is needed only during the brief
  header-parse phase. A pool of `K < N` parse buffers borrowed during parse
  and released would serve `N` connections that do not all parse at the
  same instant, trading RAM for borrow bookkeeping and a contention
  fallback. The shipped design keeps one parse slice per worker for
  simplicity.
- **Right-sizing is independent of count.** Per-listener buffers can be
  trimmed (the :80 shell is a small gzipped asset) to reclaim RAM
  regardless of the connection count.

## Per-board constraint

The buffer sizes and pool depths are per board, because the RAM budget is
not shared:

- **bmm-pico / rp2350 (RP2350) / esp32s3:** ample SRAM; the 5-watcher
  target fits at `5` per listener.
- **rp2040 / stm32f407 / stm32f439:** the tight boards, which is why their
  pools stay at `1` per listener. Raising them needs a buffer trim
  elsewhere first, and the per-board `StackResources` must be re-checked.

## How to measure

`http-load-test` drives `N` independent clients, each polling one
endpoint on a jittered interval, and sweeps the concurrency from 1 to a
ceiling, reporting success rate and latency per level. The jitter
desynchronizes the clients so the result reflects real browsers rather than
a lockstep worst case.

```sh
# Sweep 1..8 concurrent API pollers against a board, 8 polls each.
http-load-test redacted50.151 --max 8 --rounds 8 --jitter 0.8
```

Defaults: `root:root` credentials, port 8080, path `/api/info`, 2 s poll
interval, ±0.5 s jitter, and a viability threshold of 99 % success. The
"viable concurrency" line reports the highest `N` that stayed at or above
the threshold. Find the board's address from its boot log
(`ip address: …`) or over mDNS.
