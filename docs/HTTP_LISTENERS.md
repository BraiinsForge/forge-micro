# HTTP listener concurrency

Status: implemented. Scope: the `ubos-http` accept loop and its consumers.
`ubos-webui` and `ubos-api` share one listener on port 80 behind
`ubos_http::PrefixRouter`. `ubos-metrics` has its own listener on port
8081.

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

The depth is a per-board constant (`http_pool` and `metrics_http_pool` in
`ControlBoardDescriptor`) that sizes the `TcpBuffers`, the parse buffer,
and the worker task pool. Because each worker is its own task with its own static
future, the whole per-listener footprint is fixed and linker-checked; it
does not depend on how deeply any single future is polled. An earlier
design folded the whole worker pool into one `join_array` future, which
grew with the pool and overflowed the stack at boot for large pools — the
per-task structure removes that failure mode (see below).

Two properties matter for capacity:

- **The port 80 listener keeps a connection alive.** A response carries no
  `Connection: close` header, so the browser reuses one connection for the
  page assets and for every poll that follows. A worker is therefore
  pinned to the client for as long as the page is open. The metrics
  listener still closes after each scrape. An error response is safe on a
  connection that stays open, because edge-http reads the rest of the
  request body before it writes the status line, in `complete_request`.
  No unread bytes stay in the stream.
- **Each of the two waits is bounded on its own.** A worker waits at most
  `Http::IDLE_TIMEOUT` (3 s) for a request to arrive on an open
  connection, and the socket timeout `Http::SOCKET_TIMEOUT`
  (`Net::SOCKET_TIMEOUT`, 10 s) bounds progress inside one exchange. An
  upload therefore runs for as long as bytes keep arriving, and an
  abandoned browser frees its worker after 3 s. One value cannot do both
  jobs. A single 3 s aborts an OTA upload mid-sector, and a single 10 s
  lets two abandoned tabs hold the whole HTTP surface of a tight board.
  A completed exchange that asked to close gets a bounded graceful close.
  Every other exit aborts with a reset. This is what
  removed the earlier ~30 s unreachable window after a live network apply,
  where a single graceful-closing socket blocked all new connections.

Buffer sizes:

| Listener | RX | TX | request-parse | per-connection |
|----------|----|----|---------------|----------------|
| :80 `ubos_http::consts::Http` | 2 KiB | 2 KiB | 2 KiB | **6 KiB** |
| :8081 `ubos_metrics::consts::Metrics` | 512 B | 1 KiB | 512 B | **2 KiB** |

The sizes are the same on every board. What differs is where the buffers
sit: `stm32f407` and `stm32f439` put them in CCM through the
`ccmram-bss` feature of `ubos-http`, which a board reaches as
`ubos-firmware/ccmram-bss`. See "Which memory the pool sits in" below.

Shipped pool depths are per board:

| Board | `http_pool` | `metrics_http_pool` |
|-------|-------------|---------------------|
| bmm-pico, rp2350, hashnode, braiins-axe | 8 | 5 |
| esp32s3 | 4 | 3 |
| rp2040, stm32f407, stm32f439 | 2 | 1 |

Each board takes the largest depth its RAM links. Two is the floor
everywhere. One slot puts the OTA recovery path behind whatever worker
an open dashboard holds, and it also leaves a board with an open page
unreachable to every other client, because the 2 s poll holds that one
worker. The metrics listener keeps a pool of its own so a Prometheus
scrape draws its own worker instead of contending for the shared pool. A
dedicated ingest task, not the accept loop, drains the push channel into
the shared metric map.

## Capacity model

A connection that stays open holds its worker. The SPA polls every 2 s,
so a browser holds one worker for as long as the page is open. The pool
depth is therefore the ceiling on **simultaneous connections**, not on
clients.

A client is not one connection. A browser opens a second one for the
burst at page load. It is only a second one, because the `build.rs` of
`ubos-webui` puts the CSS and the JS inside the gzipped HTML, so a page
load is `/` and `/logo.svg` and nothing more. Count two connections for
each browser and one for each machine client.

`StackResources<N>` must cover the continuously held sockets plus the
TIME_WAIT backlog. The client is now the active closer, so the client
holds the TIME_WAIT and the board holds fewer of them than it did before
the merge.

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
accept. The ~24 KiB gzipped dashboard therefore takes roughly **12 round
trips**, not the one or two a 14 KiB initcwnd would imply.

The client receive window is never the bottleneck: Windows and macOS
default to ~64 KiB before window scaling, and Linux autotunes from ~64 KiB
into the MiB range (`tcp_rmem` default 128 KiB, max 6 MiB on a current
laptop). All are far above the server's 2 KiB.

Consequences:

- **The compressed-size budget is essentially academic on a LAN.** At a
  sub-millisecond LAN RTT, 12 round trips is a few milliseconds. Whether the
  page is 14 KiB or 17 KiB changes the round-trip count by one or two, not
  the user-visible load time. The "first paint must fit one 14 KiB
  initcwnd" rationale in `ubos-webui`'s size test and `build.rs` does not
  describe this firmware; the real wall is 2 KiB/RTT.
- **Do not split assets to chase the budget.** The listener now keeps the
  connection alive, so a second request reuses the warm connection and
  costs no handshake. It still costs a round trip, and it saves a byte
  count that the TX buffer already makes irrelevant.
- **The lever for load time, if it ever matters, is `TX_BUFFER_SIZE`, not
  gz size.** Raising it (RAM permitting) widens the per-connection window;
  shrinking the payload does not change the 2 KiB/RTT ceiling. Note this
  trades against the concurrency footprint, since TX buffer is per worker.

## Measured on bmm-pico (RP2350)

CAUTION: The numbers below were measured before the two listeners merged.
Do not read them as the current ceiling. They count clients against a
listener that closed after every response, which made a worker free
between polls. A worker is now held for as long as the connection is
open, so the pool depth bounds connections rather than clients. Run the
sweep again against the merged listener and replace this table.

Measured as described under *How to measure*, with clients
desynchronized by ±0.8 s of jitter:

| pre-merge `API_HTTP_POOL` | clean viewers (≥99%) | above the pool |
|---------------------------|----------------------|----------------|
| 2 | 2 | degrades gracefully to ~85% |
| 5 | 5 | degrades gracefully to ~91% at 8 |

The behaviour above the ceiling still holds. Desynchronized clients
degrade gracefully rather than collapsing, and the board stays up. An
overloaded listener drops connections. It does not crash.

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
  ~8 KiB — too small at 128 KiB main plus 64 KiB CCM. The `.ccmram.bss`
  half of that is the auth and mining statics; the pool buffers were in
  main SRAM at the time of that measurement and are in CCM now.
- **stm32f439** overflows `.ccmram.bss` by ~7 KiB and the stack by a few
  hundred bytes: ten worker futures plus the 5/5 buffers do not fit in its
  combined RAM.

The dominant cost is the per-listener worker `TaskStorage` futures, which
live in main SRAM on every board.

**A merged slot costs more than either slot it replaced.** Every slot on
port 80 must be able to serve the REST API, whose handler future is far
larger than the dashboard's. Before the merge the board paid for that
future once per API slot and paid a much smaller one per web UI slot.
Measured with `cargo size --release -- -A` across the merge:

| Board | Pools before | Pool after | main SRAM |
|-------|--------------|------------|-----------|
| rp2350 | 5 + 5 | 8 | +15 040 B |
| rp2040 | 1 + 1 | 2 | +12 240 B |
| stm32f407 | 1 + 1 | 2, buffers in CCM | −2 112 B |
| stm32f439 | 1 + 1 | 2, buffers in CCM | −14 336 B |

The merge is therefore not a way to reclaim RAM. It buys one origin, no
CORS, one request for each API call instead of two, and one acceptor. It
costs main SRAM on every board that keeps its buffers there. The two
STM32 boards come out ahead only because the move to CCM takes more off
main SRAM than the extra slot puts back.

## Which memory the pool sits in

`http_pool_static!` in `ubos-http` declares the `TcpBuffers` and the
parse area. Under `ccmram-bss` it puts them in `.ccmram.bss`. Two facts
make that safe, and both were checked in the source rather than assumed:

- **Nothing here is a DMA target.** The STM32 ethernet driver builds its
  descriptor rings from `Packet<RX_BUFFER_SIZE>` and
  `Packet<TX_BUFFER_SIZE>` arrays and from nothing else, which is the
  board's own `PACKETS` static. The socket buffers go the other way:
  `embassy_net::tcp::TcpSocket::new` hands them to a
  `smoltcp::tcp::SocketBuffer`, and smoltcp copies between the packet
  queue and them. CCM is not reachable by DMA, so a buffer that a
  descriptor ever pointed at could not move there.
- **Both statics are all-zero at rest**, which `.ccmram.bss` requires
  because the pre-init helper zeroes the whole section. `TcpBuffers` is
  a pool of `Cell<bool>` flags that start `false` over a `MaybeUninit`
  payload, and the parse area is a zeroed array.

CAUTION: On `stm32f407` the stack shares CCM with `.ccmram.bss`, so
every pool slot added there takes 6 KiB of the stack's room. `memory.x`
holds a 16 KiB floor under what is left and fails the link below it. The
board links with about 29 KiB today.

`stm32f439` keeps its stack in main SRAM, so the move grows that stack
rather than shrinking it.

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
  linker-checked, raising `http_pool` is safe to try: it either links
  (and fits) or fails the build. Validate with a release build and the load
  test.
- **Shrink the REST handler future.** It is what makes a merged slot
  expensive, and every slot pays it. The handler holds several large
  buffers on the stack across an await, so each one counts once per
  slot.
- **Hoist the request parts into the worker.** `serve_worker` can call
  `extract_request_parts` once and pass `&RequestParts` to
  `Service::handle`. Both handlers call it themselves today, and each one
  repeats the allow-list check and the `record_request` call.
- **Take a slice if a fourth listener arrives.** `min_stack_sockets`
  takes one argument for each pool, so every board main changes with the
  arity. A `min_stack_sockets(pools: &[usize])` takes that change once
  and never again. Two listeners is the shape the firmware has, so the
  positional form stays until a third pool exists.

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

- **bmm-pico, rp2350, hashnode, braiins-axe:** ample SRAM, and the merged
  pool sits at `8`.
- **esp32s3:** `4` is the deepest merged pool that clears the stack floor
  its `build.rs` link-time ASSERT enforces. The pool lives in `.bss`,
  which shares `dram_seg` with the core-0 stack.
- **rp2040, stm32f439:** tight, and at the floor of `2` with the default
  buffer sizes. Raising them needs a buffer trim first, and the per-board
  `StackResources` must be re-checked.
- **stm32f407:** the tightest, and at the floor only because its pool
  buffers sit in CCM. That memory is shared with the stack there, so
  `memory.x` holds a floor under what is left of it.

## How to measure

Drive `N` independent clients, each polling one endpoint on a jittered
interval, and sweep the concurrency from 1 to a ceiling, recording
success rate and latency per level. The jitter is what makes the result
representative: it desynchronizes the clients the way real browsers do,
where a lockstep sweep would measure only the worst case and understate
capacity.

Each client must hold one connection open across its polls. A client
that opens a fresh connection for each poll measures the model this
listener no longer has, whatever the poll interval is.

The numbers above were taken at port 8080 on `/api/info` with
`root:root`, a 2 s poll interval, ±0.5 s of jitter, and a viability
threshold of 99 % success. The API answers on port 80 now. Find the
board's address from its boot log (`ip address: …`) or over mDNS.

What the sweep bounds is the peak simultaneous connection count rather
than sustained throughput. Divide by about two to get the browser count.
