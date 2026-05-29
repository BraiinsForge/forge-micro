# API & Observability Refactor Plan

> **Status.** Historical planning artifact kept for the design rationale
> and decision log. The work was shipped across the `jkr/api` branch; in
> particular the `ubos-metrics::http` module no longer exists, so any
> path reference in the phase sections below is a snapshot of the
> pre-refactor layout and not a pointer to current code.

## Goal

Modernize `ubos-main` observability, API surface, web UI, discovery,
and security so the miner can be controlled and monitored both by a
built-in web GUI and by external devices (e.g. Braiins Deck) over a
documented REST API.

## Non-goals (scope rule)

No new functional behavior in this round. This is a pure
observability / API / architecture refactor.

- Action endpoints (pause, resume, restart, config writes) are wired
  into the API surface but only log the request and return a stub
  success response.
- Read endpoints expose data we already collect. Anything that would
  require new sensors, new control paths, or new mining logic is out
  of scope.
- No new dependencies on WiFi, OTA, or NVS concepts that do not exist
  on our Ethernet-only cortex-m + esp32s3 stack.
- Credential changes (`PATCH /api/system` with a `password` field)
  remain stubs in this round: logged, not persisted. The default
  `root:root` credentials survive every reboot.

**Acceptance for the API:** an external client can hit every
documented endpoint, get a well-formed response, and find a log line
for any state-changing call.

## Current state (audit summary)

Cited file paths are anchors for each phase.

### HTTP server

- Single hand-rolled HTTP/1.0 implementation on top of
  `embassy_net::tcp::TcpSocket` and `embedded_io_async::Write`. No
  `picoserve`, no `embedded-svc`, no `httparse`.
- All five public endpoints live in
  `ubos/ubos-metrics/src/http.rs:260-313` (port 80) plus `/metrics`
  in `ubos/ubos-metrics/src/metrics.rs:260` (port 8081).
- Request parser does not check HTTP method — `POST /api/info`
  routes identically to `GET /api/info`
  (`ubos/ubos-metrics/src/http.rs:331`).
- Per-connection buffers: 4 KiB rx, 4 KiB tx, 12 KiB response, 2 KiB
  request; all from `ubos/ubos-core/src/consts.rs:14`. Compile-time
  assert keeps the response under the 14 KiB initcwnd budget.
- Identical HTTP task runs on every firmware (rp2040, rp2350,
  bmm-pico, stm32f407, stm32f439, esp32s3, host-runner). No
  board-conditional disable.
- HTML payload: single 188-line `index.html` gzipped at build time
  by `ubos/ubos-metrics/build.rs`, included via `include_bytes!`,
  served with `Content-Encoding: gzip`, `Cache-Control: no-store`.
  Logo is a separate cacheable `image/svg+xml` asset.

### Existing endpoints

| Method | Path           | Returns                              | State? |
|--------|----------------|--------------------------------------|--------|
| any    | `/`            | gzipped SPA dashboard                | no     |
| any    | `/logo.svg`    | Braiins wordmark, cacheable          | no     |
| any    | `/api/info`    | JSON telemetry blob                  | no     |
| any    | `/api/config`  | `{"ok":true}` stub, logs body        | no     |
| GET    | `/metrics`     | Prometheus text exposition, port 8081| no     |

### Metrics pipeline

Producers push `Metric` values into a 32-deep
`embassy_sync::channel::Channel`
(`ubos/ubos-metrics/src/metrics.rs:133`). The scrape task drains
into a `FnvIndexMap<MetricKey, StoredValue, 16>` and renders into a
2 KiB response buffer. The 16-entry map cap is set at
`ubos/ubos-core/src/consts.rs:114` and will be lifted generously
in Phase 6; no per-board RAM target makes this tight.

Hardcoded metrics rendered outside the channel:
`board_uptime_seconds`, `board_http_requests_total`,
`board_btc_price_usd`, `board_btc_price_age_seconds`,
`board_ntp_synced`. Channel-pushed:
`board_chip_temperature_millicelsius`, `board_fan_pwm_percent`,
`board_fan_rpm`, `board_cpu_busy_ratio{cpu,window}`.

### Sensor data not yet exposed

| Category    | Datum                                        | Source                                                    |
|-------------|----------------------------------------------|-----------------------------------------------------------|
| Power       | PSU input/output power (mW)                  | `read_input_power_mw` / `read_output_power_mw` already called every 2 s in `ubos/ubos-psu/src/lib.rs:78`, only `info!`-logged |
| Power       | TPS6287x status flags                        | `read_status` exists at `ubos-psu/src/tps6287x.rs:257`, only called at init |
| Power       | INA230 bus voltage / shunt voltage / current | INA230 driver only reads `REG_POWER`                       |
| Temperature | Per-LM75A reading (4 sensors)                | Each sensor is read in `ubos-temperature/src/lm75a.rs:102`, only the max is returned |
| Temperature | RP2350/RP2040 die temperature                | `ADC_TEMP_SENSOR` extracted in `bmm-pico/src/config.rs:23`, never wired |
| Fan         | Sensor-fault consecutive misses              | `sensor_misses` local in `ubos-thermal/src/lib.rs:run_thermal` |
| Mining      | Hashrate (H/s)                               | `EWMA_HASHRATE` already read for `/api/info`               |
| Mining      | Total chip shares                            | `SHARE_COUNT` at `ubos-metrics/src/hashrate.rs:16`         |
| Mining      | Accepted / rejected pool shares              | Only logged in `stratum-client/src/state.rs:91`            |
| Mining      | `MINER_IS_RUNNING` state                     | superseded by `MiningHandle::state().is_some_and(|s| s.running)`, exposed via `board_miner_running` |
| Mining      | UART desync byte count                       | `dropped_byte: u32` at `ubos-mining/src/bm1362.rs:347`      |
| Mining      | Pool liveness                                | `POOL_ALIVE_BITMAP` declared at `ubos-net/src/net.rs:151`, never written |
| Network     | Stratum reconnect count                      | Loop in `stratum_client_task`, no counter                  |
| Scheduler   | bmm-pico Core 1 busy ratio                   | `IDLE_TICKS[1]` populated, `cpu_load_task` not spawned     |

### mDNS

- Implementation: `edge-mdns` (`io` feature) + `edge-nal-embassy` in
  `ubos/ubos-net/src/mdns.rs`. Two services advertised in a local
  `[Service<'_>; 2]` array built at `mdns.rs:167`:
  - `_prometheus._tcp.local.` on port 8081 with `path=/metrics`,
    `board=…`, `mcu=…` TXT records.
  - `_http._tcp.local.` on port 80 with `path=/`, `board=…`,
    `mcu=…`.
- Hostnames: per-board `MDNS_HOSTNAME_BASE` const in each
  firmware's `config.rs`, suffixed with the last MAC byte by
  `compose_hostname` (`mdns.rs:286`).
- No compile-time registry exists anywhere in the workspace. No
  `inventory`, `linkme`, `distributed_slice`, `#[link_section]`,
  or macro-rules registry pattern is used.

### Security posture

- HTTP server: no auth, no CSRF, no Origin check, no method
  allowlist, no IP filter, no TLS, no rate limit, no request-size
  cap beyond `REQUEST_BUFFER_SIZE = 2048`. The handler at
  `ubos/ubos-metrics/src/http.rs:289` already documents
  `// '/api/config' is unauthenticated`.
- Prometheus server: same, plus path-equality check on `/metrics`.
- `embedded-tls` is a dependency but used only as an outbound TLS
  client for the Binance BTC fetch.

### reference-miner OpenAPI as a starting point

Reference spec at
<https://github.com/example/reference-miner/blob/master/main/http_server/openapi.yaml>.
The reference-miner spec is the **shape** we copy (paths, verbs, rough
field set), but the wire schema is ours: we publish
`apiVariant: "ubos-0.1"` on every response that has a version field,
and Braiins Deck is expected to implement this API rather than
the reverse. Where reference-miner constrains a field to a fixed enum
(`ASICModel`), we widen it to a free-form string so future chips
ship without a spec bump.

| Method | Path                                | Map to us                                              |
|--------|-------------------------------------|--------------------------------------------------------|
| GET    | `/api/system/info`                  | implement, omit ESP-only fields                        |
| GET    | `/api/system/asic`                  | implement; `ASICModel` widened to free-form string, value `"BM1362"` |
| GET    | `/api/system/statistics`            | implement against `ubos-metrics` time-series ringbuffer (already in `index.html` JS) |
| GET    | `/api/system/scoreboard`            | implement; back with new bounded ring populated from `SubmitResult::Accepted` events |
| GET    | `/api/system/logs`                  | stub; defer until defmt-to-text bridge exists          |
| GET    | `/api/system/wifi/scan`             | **out** — Ethernet stack, returns `501 Not Implemented` |
| POST   | `/api/system/pause`                 | stub: log + 200                                        |
| POST   | `/api/system/resume`                | stub: log + 200                                        |
| POST   | `/api/system/restart`               | stub: log + 200 (wire to `SCB::sys_reset` later)       |
| POST   | `/api/system/identify`              | stub: log + 200                                        |
| POST   | `/api/system/blockFound/dismiss`    | stub: log + 200                                        |
| PATCH  | `/api/system`                       | stub: log body + 200                                   |
| POST   | `/api/system/OTA`                   | **out** — return `501 Not Implemented`                 |
| POST   | `/api/system/OTAWWW`                | **out** — return `501 Not Implemented`                 |

reference-miner's spec defines no `securitySchemes` and no CSRF header;
state-changing endpoints return `401 Unauthorized` with the message
`"Client not in allowed network range"`. The spec's security model
is therefore an IP allow-list only. Our CSRF requirement comes from
the AxeOS guide and is additive.

## Target crate layout

```
ubos/
  ubos-http/        NEW — generic HTTP/1.0 server primitives
                          (accept loop, request parser, response writer,
                           BufferWriter, method+path routing)
  ubos-api/         NEW — REST endpoint handlers, schema types
                          (depends on ubos-http, ubos-metrics, ubos-net,
                           ubos-mining-state, ubos-mdns-registry)
  ubos-webui/       NEW — bundled SPA assets (index.html.gz, logo.svg,
                          future CSS/JS), served by ubos-api
  ubos-mdns/        NEW — extracted from ubos-net, owns the responder
                          loop and the compile-time service registry
                          macro
  ubos-metrics/     unchanged role: Prometheus exporter + cpu_load,
                          but the HTTP server moves out
  ubos-net/         unchanged role: DHCP, NTP, BTC, atomics
                          (mdns module deleted)
```

Each new crate registers an mDNS service at the call site that owns
the port, via a single declarative macro defined in `ubos-mdns`.

## Compile-time service registry mechanism

Pick `linkme` (`distributed_slice`). Rationale:

- Works in `no_std` on every target we ship (cortex-m, xtensa, host).
  `inventory` needs a runtime constructor and does not run on bare
  cortex-m; `linkme` only needs the linker to merge sections, which
  every target's linker already does.
- The workspace uses none of these today, so this is the one new
  workspace-wide dependency. Adding it gets a separate
  `Cargo.toml`+`Cargo.lock` commit per repo convention.
- A const-slice fallback (single hand-written `const REGISTRY:
  &[ServiceEntry] = &[…]` in `ubos-mdns`) stays documented in case
  `linkme` ever breaks a target's link script. Not the default.

Declaration sketch (final API decided in Phase 7):

```rust
// in ubos-api/src/lib.rs
use ubos_mdns::{declare_service, ServiceEntry};

declare_service!(HTTP_API_MDNS = ServiceEntry {
    service: "_http._tcp.local.",
    port: ubos_core::consts::Http::PORT,
    txt: &[("path", "/"), ("api", "reference-miner-1.0")],
});
```

`ubos-mdns::declare_service!` expands to a
`#[distributed_slice(ubos_mdns::REGISTRY)]` static. The responder
loop iterates `REGISTRY` to build the SRV/TXT advertisement set.

## Phases

Each phase is one logical change, gated on user approval. Commits
inside a phase follow the repo's atomic-commit rules: dep churn
(`Cargo.toml`+`Cargo.lock`) precedes consumer churn, generated
assets (`index.html.gz`) get their own commit, `.claude/` config is
never bundled with code.

### Phase 1 — Extract HTTP primitives into `ubos-http`

**Scope.** Pure refactor. No endpoint changes. No behavior changes.

**Work.**

1. Create `ubos/ubos-http/` with `Cargo.toml` and a `cargo check` +
   `cargo clippy -- -D warnings` derivation in `flake.nix` checks.
2. Move from `ubos/ubos-metrics/src/http.rs`:
   - `BufferWriter` (currently `pub(crate)`, referenced from
     `metrics.rs` via `crate::http::BufferWriter`).
   - The accept loop skeleton (socket bind, accept, read, dispatch,
     write).
   - `parse_request_path` and `extract_request_body`.
3. Introduce a trait `HttpService` with one method:
   `async fn handle(&self, req: &Request<'_>, w: &mut Writer<'_>) ->
   Result<(), Error>`. The trait is the seam between `ubos-http`
   (transport) and `ubos-api` (handlers).
4. `ubos-metrics` keeps the Prometheus port 8081 server but now
   uses `ubos-http` primitives. `run_http_server` for port 80 also
   uses `ubos-http`; concrete handlers stay where they are for now.
5. Methods are checked: `POST /api/info` returns `405`. This is the
   first user-visible deviation from current behavior, but the
   existing SPA only issues `GET` and `POST` to the right paths, so
   no regression is expected.

**Acceptance.**

- `nix flake check` green on all six firmware crates.
- Manual smoke test: load `/`, watch `/api/info` poll, POST
  `/api/config`, scrape `/metrics`.

**Approval gate.** Stop and wait for user sign-off before Phase 2.

### Phase 2 — Extract `ubos-api`

**Scope.** Move the existing four handlers (`/`, `/logo.svg`,
`/api/info`, `/api/config`) into a new `ubos-api` crate that
implements `HttpService`. No new endpoints.

**Work.**

1. Create `ubos/ubos-api/`, dep on `ubos-http`, `ubos-net` (for the
   atomics it reads), `ubos-core` (for fan state), `ubos-metrics`
   (for the EWMA hashrate getter).
2. Move handlers verbatim. `REQUEST_COUNT` increment moves with the
   handler.
3. Each firmware's `main.rs` now spawns
   `ubos_api::run(stack, …).await` instead of
   `ubos_metrics::http::run_http_server`. `ubos_metrics` only owns
   the Prometheus task.
4. Promote `BufferWriter` to `ubos-http`'s public API (no longer
   `pub(crate)`).

**Acceptance.**

- Identical wire behavior: bytes-on-the-socket diff between Phase 1
  and Phase 2 is empty for every endpoint.
- `nix flake check` green.

**Approval gate.**

### Phase 3 — Extract `ubos-webui`

**Scope.** Move static assets out of `ubos-metrics` so the UI can
evolve independently of the API.

**Work.**

1. Create `ubos/ubos-webui/` containing `src/index.html`,
   `src/logo.svg`, and `build.rs` (moved verbatim from
   `ubos-metrics/build.rs`).
2. Expose two consts: `pub const INDEX_HTML_GZ: &[u8]` and
   `pub const LOGO_SVG: &[u8]`.
3. `ubos-api` depends on `ubos-webui` for these consts. The two
   handlers (`/`, `/logo.svg`) read from there.
4. `ubos-metrics/build.rs` is deleted.

**Acceptance.**

- Single-commit move (no dependency churn since `flate2` is a
  build-dep that moves with the crate).
- `nix flake check` green.

**Approval gate.**

### Phase 4 — REST read endpoints (reference-miner compatible)

**Scope.** Add the GET endpoints from the reference-miner spec, backed by
existing readings. Old `/api/info` stays alive in parallel until
the SPA is migrated.

**Work.**

1. Define schema types in `ubos-api/src/schema/` using
   `serde::Serialize` against `serde-json-core` (already in the
   workspace via stratum). One module per response type:
   `system_info.rs`, `system_asic.rs`, `system_statistics.rs`,
   `system_scoreboard.rs`.
2. Implement:
   - `GET /api/system/info` — populate everything we have plus
     `apiVariant: "ubos-0.1"`. ESP-only fields (`idfVersion`,
     `freeHeapSpiram`, `wifiStatus`, …) are omitted. Clients
     consume this through the `apiVariant` discriminator, not by
     pretending we are a reference-device.
   - `GET /api/system/asic` — `ASICModel` is a free-form string,
     value `"BM1362"`. `frequencyOptions` and `voltageOptions`
     come from `ubos-mining` consts. The schema is ours; Deck
     implements it.
   - `GET /api/system/statistics` — adapt the existing ring buffer
     that already feeds the SPA into the spec's `{currentTimestamp,
     labels[], statistics[][]}` shape. Honor `?columns=`.
   - `GET /api/system/scoreboard` — add a bounded
     `BoundedHeap<SubmitResult, 20>` in `ubos-metrics` (or
     `ubos-mining-stats`, decide in implementation) populated from
     `visit_submit_result` in `stratum-client/src/state.rs:91`. No
     new ASIC data, just bookkeeping that already passes through
     the same function.
3. `GET /api/system/wifi/scan` and the two `POST .../OTA*`
   endpoints return `501 Not Implemented` with a JSON
   `{"message":"unsupported on this platform"}` body.
4. `GET /api/system/logs` returns `501 Not Implemented`.

**Acceptance.**

- Each new endpoint returns a parseable response under the 12 KiB
  buffer. Schema lint: every `Serialize` struct round-trips through
  `serde-json-core` in a host-side unit test.
- Existing `/api/info` and `/api/config` still work.

**Approval gate.**

### Phase 5 — REST action endpoints (stubs)

**Scope.** Wire the spec's state-changing endpoints to the router,
returning 200 + a log line. No firmware-side action is taken.

**Work.**

1. Implement `POST /api/system/pause`, `/resume`, `/restart`,
   `/identify`, `/blockFound/dismiss`, and `PATCH /api/system` as
   handlers that:
   - Parse the body (for PATCH).
   - Emit a single `info!` log line with the path, parsed body
     summary, and remote IP if available.
   - Return `200 OK` with `GenericResponse { message: "ack" }`.
2. Method enforcement: every state-changing endpoint requires its
   exact method or returns `405`.

**Acceptance.**

- External client (curl) hits all six endpoints, gets 200, finds
  one log line per call.
- No firmware state visibly changes.

**Approval gate.**

### Phase 6 — Metrics coverage (expand `MAX_METRICS`, push existing readings)

**Scope.** Surface the data identified in the "Sensor data not yet
exposed" table. Each addition is its own commit.

**Work.**

1. Bump `Metrics::MAX_METRICS` in `ubos-core/src/consts.rs:114`
   from 16 to **128**. This grows the `FnvIndexMap` static; the
   cost is bounded by `sizeof(MetricKey) + sizeof(StoredValue)`
   per slot, conservatively ~64 B → ~8 KiB total. Even on the
   narrowest target (rp2040, 256 KiB RAM) that is well under 4 %
   of available RAM. Single dedicated commit because the new
   bound is the enabling change for the next set. The intent is
   to stop thinking about the cap; future metric additions do not
   need to revisit this constant.
2. Per-metric commits (each a one-shot push at the existing read
   site, no new wiring):
   - `board_psu_input_power_mw`, `board_psu_output_power_mw` —
     `ubos-psu/src/lib.rs:78`.
   - `board_psu_status_*` (current_limit, thermal_shutdown,
     overvoltage, undervoltage) — poll `read_status` in
     `run_power_stats`.
   - `board_lm75a_temperature_millicelsius{addr="0x49"…"0x4c"}` —
     `ubos-temperature/src/lm75a.rs:102`, label per sensor.
   - `board_hashrate_hps` — read `hashrate()` in a dedicated
     pusher task. Same data the SPA already shows.
   - `board_shares_total`, `board_shares_accepted_total`,
     `board_shares_rejected_total` — counters incremented in
     `visit_submit_result`.
   - `board_miner_running` — peek the `Signal` or maintain a
     mirrored `AtomicBool`.
   - `board_uart_desync_bytes_total` — promote
     `ResponseParser::dropped_byte` to a shared atomic.
   - `board_pool_connected` — wire `POOL_ALIVE_BITMAP` write from
     `stratum-client`.
   - `board_stratum_reconnects_total` — counter in the reconnect
     loop.
   - `board_cpu_busy_ratio{cpu="1"}` on bmm-pico — spawn
     `cpu_load_task::<2>` instead of the current Core 0-only
     variant.

Total target additions: 16 new metrics.

**Acceptance.**

- `/metrics` on each board emits all new lines. Existing Grafana
  dashboard does not regress.
- `nix flake check` green.

**Approval gate.** This phase has many commits; the gate is after
the whole set lands.

### Phase 7 — `ubos-mdns` crate with compile-time registry

**Scope.** Decouple mDNS from `ubos-net`. Each service-owning crate
registers its own entry.

**Work.**

1. Create `ubos/ubos-mdns/`. Move `mdns.rs` from `ubos-net`. Add
   `linkme` to workspace deps (separate `Cargo.toml`+`Cargo.lock`
   commit).
2. Define `pub struct ServiceEntry { service: &'static str, port:
   u16, txt: &'static [(&'static str, &'static str)] }` and
   `#[linkme::distributed_slice] pub static REGISTRY:
   [ServiceEntry]`.
3. Provide `declare_service!` macro. Document that side effect
   (every register must be `pub` so the slice gathers across crate
   boundaries).
4. `ubos-api` declares its `_http._tcp.local.` entry.
5. `ubos-metrics` declares its `_prometheus._tcp.local.` entry.
6. The current `Service<'_>` array in `mdns.rs:167` becomes a
   compile-time view over `REGISTRY`. The fallback bare-const path
   (see "Compile-time service registry mechanism" above) is left
   in place behind a feature flag in case `linkme` breaks an
   unusual target.
7. Hostnames stay in each firmware's `config.rs` because they
   depend on MAC bytes, not on services.

**Acceptance.**

- `avahi-browse -art` from a Linux host shows both services on
  every board, unchanged from today.
- Removing the `ubos-api` dep from a future custom firmware
  automatically removes the HTTP mDNS entry. Verify with a
  one-shot test build.

**Approval gate.**

### Phase 8 — Login, CSRF tokens, IP allow-list

**Scope.** Three independent security layers, applied in this
order on each request: IP allow-list → authentication → CSRF.
Default credentials are hardcoded `root:root`; credential changes
through `PATCH /api/system` remain stubs in this round (logged but
not persisted), per the scope rule.

**Auth model.**

- **GUI (browsers):** session cookie. `GET /login` returns a
  small HTML form (same gzip-bundled asset pipeline as
  `index.html`). `POST /login` validates credentials, on success
  issues `Set-Cookie: session=<token>; HttpOnly; SameSite=Strict;
  Path=/`. Subsequent requests carry the cookie. `POST /logout`
  invalidates the session.
- **REST API (machine clients, e.g. Braiins Deck):** HTTP Basic
  auth on every request. Simpler for clients than a login round
  trip; the `Authorization: Basic …` header is checked against
  the same credential store.
- A single `pub static CREDENTIALS: Mutex<Credentials>` in
  `ubos-api`, initialized to `root:root`. Both auth paths read
  from this one value.

**CSRF interaction.**

- Required on every state-changing (`POST`, `PATCH`, `DELETE`)
  request that arrives with a session cookie. The login `POST
  /login` itself is exempt (it is the bootstrap).
- Not required for requests authenticated via HTTP Basic, because
  Basic auth does not ride on ambient browser credentials.
- Token issued in the `X-CSRF-Token` response header on `GET /`,
  `GET /login`, `GET /api/system/info`. Stored in
  `Mutex<heapless::Vec<Token, 16>>` with a 5-minute TTL.

**Work.**

1. **IP allow-list.** `pub const ALLOW_LIST: &[Ipv4Cidr]` in
   `ubos-api`. Default: the four standard private ranges
   (`10.0.0.0/8`, `172.16.0.0/12`, `192.168.0.0/16`,
   `169.254.0.0/16`). Off-list requests get `401 Unauthorized`
   with body `{"message":"Client not in allowed network range"}`
   (exact reference-miner wording, for client compatibility). This is a
   network-layer screen — it runs before any auth check.
2. **Credential store.** Hardcoded `Credentials { username:
   "root", password: "root" }` const. Mutable copy lives in a
   `Mutex<Credentials>` so the future PATCH endpoint can replace
   it (no persistence in this round; reboot returns to defaults).
3. **GUI login page.** New `ubos-webui` asset `login.html`, also
   gzip-bundled by `ubos-webui/build.rs`. Form posts to `/login`.
4. **Session table.** `Mutex<heapless::FnvIndexMap<Token,
   SessionMeta, 8>>` in `ubos-api`. 8 concurrent sessions is
   enough for a single-user device; the map evicts the oldest
   entry on insert. `SessionMeta` records the issue time for
   24-hour TTL.
5. **Auth middleware.** Single helper
   `authenticate(req) -> Result<AuthCtx, AuthError>` that:
   - First tries `Authorization: Basic …` → returns
     `AuthCtx::Basic`.
   - Then tries the `session` cookie → returns
     `AuthCtx::Session(token)`.
   - Else returns `AuthError::Unauthenticated` (`401`,
     `WWW-Authenticate: Basic realm="ubos"`).
6. **CSRF token issuance.** As described above. Tokens are
   16 bytes from the board's hardware RNG
   (`embassy_rp::clocks::RoscRng`, STM32 RNG peripheral, ESP32-S3
   `esp_hal::rng::Rng`). Document the per-board source in the
   schema module.
7. **CSRF token check.** For session-authenticated state-changing
   requests: header `X-CSRF-Token` must match an unexpired entry.
   Missing or unknown gets `403 Forbidden` with
   `{"message":"csrf"}`.
8. **Login bypass for `/`.** Unauthenticated requests to `/` get
   `302 Found` to `/login`. `/login`, `/logo.svg`, and `/metrics`
   are reachable without auth; everything else under `/api/*`
   requires auth.
9. **Remote-IP read.** `embassy-net 0.9.0` (our pin) exposes
   `TcpSocket::remote_endpoint(&self) -> Option<IpEndpoint>` at
   `src/tcp.rs:400`. The IP is threaded through `AuthCtx` so the
   allow-list check has access to it. No patch required.

**Acceptance.**

- `curl -X POST http://<miner>/api/system/pause` from an
  off-LAN host returns `401` with the network-range message.
- `curl -u root:root -X POST http://<miner>/api/system/pause`
  from a LAN host returns `200`.
- Browser at `http://<miner>/` redirects to `/login`. Submitting
  `root` / `root` lands on `/` with a session cookie.
- A subsequent `POST /api/system/pause` from the browser without
  `X-CSRF-Token` returns `403`; with a token from `GET
  /api/system/info` it returns `200`.
- `curl -u root:root -X PATCH http://<miner>/api/system -d '{…}'`
  logs the body (existing stub behavior) and returns `200`
  **without** a CSRF token, because Basic auth is exempt.

**Approval gate.**

### Phase 9 — Per-board feasibility validation

**Scope.** Build all six firmwares with the full refactor and
record the size impact.

**Work.**

1. Capture baseline `cargo size --release` before Phase 1, store
   in `docs/API_REFACTOR_SIZES.md`.
2. After Phase 8 lands, re-run on every firmware. Update the doc.
3. If any board exceeds 95% flash or RAM, open a follow-up issue
   and pause the merge to consider feature gating
   `_OTA_NOT_IMPLEMENTED` stub responses behind a build feature
   for that board.

Targets and headrooms from the audit:

| Board       | Flash    | RAM (main + extras)              |
|-------------|----------|----------------------------------|
| bmm-pico    | 2048 KiB | 512 KiB + 4 KiB + 4 KiB          |
| rp2040      | ~2048 KiB| 256 KiB                          |
| rp2350      | 2048 KiB | 512 KiB + 4 KiB + 4 KiB          |
| stm32f407   | 1024 KiB | 128 KiB + 64 KiB SRAM2 + 64 KiB CCM |
| stm32f439   | 2048 KiB | 192 KiB + 64 KiB SRAM2 + 64 KiB CCM |
| esp32s3     | ~8 MiB   | 320 KiB DRAM + 192 KiB IRAM (+PSRAM if used) |

The rp2040 is the narrow RAM target. The 32-slot metric map adds
~512 bytes; the new schema serializer state is on-stack; the
biggest risk is `linkme` failing to merge sections on a specific
ldscript. Phase 1 already pays the bulk of the cost via the
generic `ubos-http` writer state.

**Approval gate.** This is the last phase. After approval, the
branch enters `/branch-cleanup` and the user pushes when ready.

## Workstream → phase mapping

| Workstream                              | Phases     |
|-----------------------------------------|------------|
| W1 Metrics coverage                     | 6          |
| W2 Modularize web GUI and API           | 1, 2, 3    |
| W3 Proper REST API                      | 4, 5       |
| W4 mDNS with compile-time registry      | 7          |
| W5 Rudimentary security                 | 8          |
| W6 External-client requirement (cross.) | 4, 5, 7, 8 |

## Decisions

The earlier draft listed four open questions. All are now resolved:

1. **Braiins Deck client (Phase 4).** Deck implements our API; we
   do not bend the schema to match a real reference-device. The schema is
   ours to define. The `ASICModel` field is a free-form string
   carrying `"BM1362"` and any other chip we add later, not a
   fixed enum.
2. **API variant identifier (Phase 4).** Every response that
   includes a version field carries `apiVariant: "ubos-0.1"`.
   The `0.1` is the schema's semver; breaking changes bump the
   minor while the API is pre-1.0, then the major after.
3. **Compile-time registry mechanism (Phase 7).** `linkme` +
   `declare_service!` macro is the chosen mechanism. The
   const-slice fallback stays documented in case `linkme` ever
   breaks a target, but is not the default.
4. **Remote-peer IP (Phase 8).** Resolved without a spike:
   `embassy-net 0.9.0` (our pin) exposes
   `TcpSocket::remote_endpoint(&self) -> Option<IpEndpoint>` at
   `src/tcp.rs:400`. The allow-list reads the peer IP directly
   from the accepted socket; no vendored patch needed.

## Out of scope for this plan

- Full SPA rewrite. The current `index.html` keeps working; any
  client-side polish is a follow-up plan.
- TLS for the HTTP server. Login is included in Phase 8, but the
  transport stays plain HTTP — credentials and CSRF tokens travel
  in the clear on the LAN. A future plan can layer
  `embedded-tls` on top.
- Persistent credential storage. The `root:root` defaults are
  hardcoded and any `PATCH` to change them is logged but not
  durable.
- ESP-IDF OTA, WiFi, or NVS feature parity.
- Time-series persistence across reboots.
- A new sensor BOM (the metrics workstream uses what is already on
  every board).
