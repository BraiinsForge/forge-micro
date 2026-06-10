# REST API Reference

Documents the HTTP surface served by the firmware. Targets two audiences:
operators driving the on-device SPA and machine clients (e.g. Braiins
Deck) speaking REST. The schema is ours, not reference-device's; the shape mirrors
reference-miner where compatibility costs nothing.

## Servers

| Port | Crate          | Purpose                                                |
|------|----------------|--------------------------------------------------------|
| 80   | `ubos-webui`   | dashboard SPA, login, logout, logo                     |
| 8080 | `ubos-api`     | JSON REST API (`/api/*`)                               |
| 8081 | `ubos-metrics` | Prometheus text-format exposition at `/metrics`        |

Discoverable via mDNS: `_http._tcp.local.` (port 80),
`_ubos._tcp.local.` (port 8080), `_prometheus._tcp.local.` (port 8081).

## Crate map

| Crate          | Role                                                                      |
|----------------|---------------------------------------------------------------------------|
| `ubos-http`    | TCP accept loop, per-connection state machine, `Service` trait; generic response helpers (`read_body`, `write_json_response`, `MAX_RESPONSE_HEADERS`) |
| `ubos-auth`    | shared HTTP middleware: allow-list, auth, sessions, CSRF, CSPRNG          |
| `ubos-api`     | REST handlers, `REST_API_SERVICE` mDNS entry, schema types                |
| `ubos-webui`   | gzipped `index.html` / `login.html`, raw `logo.svg`; webui listener; `WEBUI_SERVICE` mDNS entry |
| `ubos-mdns`    | DNS-SD responder; per-service registration via `declare_service!`         |
| `ubos-metrics` | Prometheus exporter task and metric storage                               |

`ubos_webui::run` (`ubos-webui/src/dashboard.rs`) is the firmware
entry point for port 80. `ubos_api::run_rest_api`
(`ubos-api/src/rest.rs`) is the entry point for port 8080.
`ubos_metrics::run` (`ubos-metrics/src/metrics.rs`) is the entry point
for port 8081. The old `ubos-api::run` / `dashboard::run` no longer
exists as a public name; it moved to `ubos-webui`.

## Request lifecycle

### Webui listener (port 80)

Every request on port 80 passes through, in order:

1. **IP allow-list** (`ubos-auth/src/allow_list.rs`). Off-list peers get
   `401` with body `{"message":"Client not in allowed network range"}`,
   before any auth attempt.
2. **Authentication** (`ubos_auth::auth::authenticate`,
   `ubos-auth/src/auth.rs`). Anonymous routes (`/login`, `/logo.svg`)
   skip. Everything else requires either `Authorization: Basic …` or a
   valid `session=` cookie. Failure: `401`,
   `WWW-Authenticate: Basic realm="ubos"`.
3. **CSRF** (session-authenticated, state-changing only). `POST` from a
   session client carrying a prior session cookie must include an
   `X-CSRF-Token` header matching a token bound to that session. Basic
   clients are exempt; anonymous `POST /login` (no active session cookie)
   is exempt. Failure: `403`, `{"message":"CSRF token invalid or missing"}`.
4. **Routing**. Known paths: `/`, `/login`, `/logout`, `/logo.svg`.
   Wrong method on a known path → `405`. Unknown path → `404`.

### REST listener (port 8080)

Every request on port 8080 passes through, in order:

1. **IP allow-list** (`ubos-auth/src/allow_list.rs`). Same check as
   port 80.
2. **CORS preflight**. `OPTIONS /api/*` is handled before auth so
   browsers can probe cross-origin before presenting credentials. See
   the CORS section below for the headers attached.
3. **Authentication** (`ubos_auth::auth::authenticate`,
   `ubos-auth/src/auth.rs`). Every route on port 8080 requires auth;
   there are no anonymous endpoints. Anonymous requests receive `401`
   with CORS headers attached.
4. **CSRF** (session-authenticated, state-changing only). `POST`,
   `PATCH`, `DELETE` from a session client must carry an `X-CSRF-Token`
   header matching a token bound to that session. Basic clients are
   exempt. Failure: `403`,
   `{"message":"CSRF token invalid or missing"}`.
5. **Routing**. `/api/system/*` is dispatched first (`system::dispatch`),
   then `/api/info` and `/api/config`. Wrong method on a known path →
   `405`. Unknown path → `404`.

## Endpoints

`Auth` column: **none** = anonymous, **any** = Basic or session,
**+CSRF** = session also needs `X-CSRF-Token`.

### Pages (port 80)

| Method | Path           | Auth        | Description                                                       |
|--------|----------------|-------------|-------------------------------------------------------------------|
| GET    | `/`            | session     | Dashboard SPA; gzipped; issues fresh `X-CSRF-Token` header        |
| GET    | `/login`       | none        | Login form; gzipped                                               |
| POST   | `/login`       | none        | Form `username=&password=`; sets `session=` cookie on success     |
| POST   | `/logout`      | session     | Invalidates session, clears cookie                                |
| GET    | `/logo.svg`    | none        | Braiins wordmark; `Cache-Control: public, max-age=86400`          |

Anonymous `GET /` is rewritten to `302 Found → /login`.

### REST API (port 8080)

| Method | Path           | Auth        | Description                                                       |
|--------|----------------|-------------|-------------------------------------------------------------------|
| GET    | `/api/info`    | any         | Telemetry blob consumed by the SPA poll                           |
| GET    | `/api/config`  | any         | `ConfigUpdate` snapshot: `pool`, `thermal`, `cooling_target_temp_c`, `tuning`, `api` (password blanked) |
| POST   | `/api/config`  | any +CSRF   | Partial `ConfigUpdate` (any subset of those fields); returns `{"ok":true}` |

### `/api/system/*` (port 8080)

| Method | Path                              | Auth      | Status | Description                                                              |
|--------|-----------------------------------|-----------|--------|--------------------------------------------------------------------------|
| GET    | `/api/system/info`                | any       | 200    | `SystemInfo`; full telemetry with `apiVariant: "ubos-0.1"`               |
| GET    | `/api/system/asic`                | any       | 200    | `MiningInfo`; chip facts (`asicModel`, `deviceModel`, `swarmColor`, `asicCount`), tuning defaults, `bounds: { frequencyMhz: { start, end }, voltageMv: { start, end } }` |
| GET    | `/api/system/thermal`             | any       | 200    | `ThermalInfo`; accepted ranges as `bounds: { sampleIntervalMs, dangerousTempC, cooldownTempC }` |
| GET    | `/api/system/cooling`             | any       | 200    | `CoolingInfo`; accepted ranges as `bounds: { targetTempC }` |
| GET    | `/api/system/statistics`          | any       | 200    | `SystemStatistics`; label set + (currently empty) ring of samples        |
| GET    | `/api/system/scoreboard`          | any       | 200    | `SystemScoreboardEntry[]`; currently empty                               |
| GET    | `/api/system/logs`                | any       | 501    | `{"message":"not implemented"}`                                          |
| GET    | `/api/system/wifi/scan`           | any       | 501    | `{"message":"unsupported on this platform"}`                             |
| POST   | `/api/system/OTA`                 | any       | 501    | `{"message":"unsupported on this platform"}`                             |
| POST   | `/api/system/OTAWWW`              | any       | 501    | `{"message":"unsupported on this platform"}`                             |
| POST   | `/api/system/pause`               | any +CSRF | 200    | Stub: logs + sends `Manager::Stop`; returns `{"message":"ack"}`          |
| POST   | `/api/system/resume`              | any +CSRF | 200    | Stub: logs + sends `Manager::Start`; returns `{"message":"ack"}`         |
| POST   | `/api/system/restart`             | any +CSRF | 200    | Stub: logs only; returns `{"message":"ack"}`                             |
| POST   | `/api/system/factory_reset`       | any +CSRF | 200    | Sends `FactoryReset` to the config task; writes the ack, then the config task wipes persisted config to defaults and requests the reboot once persisted |
| POST   | `/api/system/identify`            | any +CSRF | 200    | Stub: logs only; returns `{"message":"ack"}`                             |
| POST   | `/api/system/blockFound/dismiss`  | any +CSRF | 200    | Stub: logs only; returns `{"message":"ack"}`                             |
| PATCH  | `/api/system`                     | any +CSRF | 200    | Stub: logs body length; returns `{"message":"ack"}`. Not persisted.      |

Stubs are wired to the router and return the documented response so
external clients can integrate against the full surface. Action handlers
live in `ubos-api/src/system.rs` (`handle_action` and `write_ack_response`);
PATCH lands in `handle_patch_system` in the same file.

### Metrics (port 8081)

| Method | Path       | Description                                                |
|--------|------------|------------------------------------------------------------|
| GET    | `/metrics` | Prometheus text exposition (`text/plain; version=0.0.4`)   |

No auth, no allow-list. Any other path → `404`; any other method on
`/metrics` → `405 Allow: GET`.

## Schemas

`/api/system/*` response structs derive `serde::Serialize` with
`#[serde(rename_all = "camelCase")]`. ubos-api-owned schemas live in
their own modules under `ubos-api/src/schema/`; `MiningInfo`,
`ThermalInfo`, and `CoolingInfo` live in `ubos-config` (alongside the
bounds they carry) and are served at `/api/system/asic`,
`/api/system/thermal`, and `/api/system/cooling`.
`ConfigUpdate` (the `/api/config` GET response and POST body) uses
serde's default snake_case naming so the wire shape matches the
ubos-config field names; consumers of `/api/config` decode keys like
`sample_interval_ms` and `cooling_target_temp_c`. Bodies are
serialized with `serde-json-core` into a 2 KiB stack scratch buffer
(`ubos-api/src/system.rs`).

| Struct                  | Endpoint(s)                                        |
|-------------------------|----------------------------------------------------|
| `SystemInfo`            | `GET /api/system/info`                             |
| `MiningInfo`            | `GET /api/system/asic`                             |
| `ThermalInfo`           | `GET /api/system/thermal`                          |
| `CoolingInfo`           | `GET /api/system/cooling`                          |
| `SystemStatistics`      | `GET /api/system/statistics`                       |
| `SystemScoreboardEntry` | `GET /api/system/scoreboard`                       |
| `ConfigUpdate`          | `GET /api/config`, `POST /api/config`              |
| `GenericResponse`       | action stubs, 501 responses, errors                |

`API_VARIANT = "ubos-0.1"` (`ubos-api/src/schema/mod.rs:24`) ships on
every versioned response. Clients dispatch on this string rather than
treating ubos as a reference-device; the minor bumps on each schema-breaking
change while pre-1.0.

## Authentication

Two paths share a single credential store
(`pub static CREDENTIALS` at `ubos-auth/src/auth.rs:112`, initialised
to `root` / `root` by `init_credentials`).

**HTTP Basic** — `Authorization: Basic <base64>`. Stateless,
re-validated per request, exempt from CSRF. Use this for machine
clients. Credentials are compared in constant time.

**Session cookie** — `POST /login` with a form body issues a 32-char
base64url token in `Set-Cookie: session=<token>; HttpOnly;
SameSite=Strict; Path=/`. The token is looked up in a fixed
`FnvIndexMap<_, _, 8>` with 24 h TTL; the oldest entry is evicted on
overflow so a tab refresh cannot lock out parallel tabs. See
`ubos-auth/src/session.rs`.

Credentials are plaintext, not hashed, and not persisted across reboot.
`PATCH /api/system` is a stub; the defaults reapply on every boot.

## CSRF

State-changing requests from session clients carry `X-CSRF-Token`.
The token is minted on `GET /` (single-use, bound to the issuing
session, 1 h TTL) and stored in `FnvIndexMap<_, _, 16>` at
`ubos-auth/src/csrf.rs`. Mismatched session id or expired token → `403`.

Basic auth bypasses the check; the `Authorization` header already
proves identity per request without any ambient browser credentials to
forge.

## CORS

The REST listener (port 8080) echoes the `Origin` request header as
`Access-Control-Allow-Origin` and unconditionally appends
`Access-Control-Allow-Credentials: true` when an `Origin` is present.
This lets the SPA loaded from port 80 issue cross-origin `fetch` calls
to port 8080 with the `session=` cookie attached.

Preflight `OPTIONS /api/*` responses additionally carry:

- `Access-Control-Allow-Methods: GET, POST, PATCH, OPTIONS`
- `Access-Control-Allow-Headers: Authorization, X-CSRF-Token, Content-Type`
- `Access-Control-Max-Age: 86400`

The IP allow-list is the primary security boundary: only clients on the
RFC 1918 LAN can reach the REST surface at all. CORS headers on top
unblock the browser's same-origin restriction for the SPA without
widening network access.

Machine clients (no `Origin` header) receive no CORS headers; the
preflight logic is skipped entirely.

## IP allow-list

Network-level screen applied before auth (`ubos-auth/src/allow_list.rs`).
Default ranges (`DEFAULT_ALLOW_LIST`):

- `10.0.0.0/8`
- `172.16.0.0/12`
- `192.168.0.0/16`
- `169.254.0.0/16`

IPv4-mapped IPv6 (`::ffff:x.x.x.x`) is unwrapped and checked as IPv4.
Native IPv6 is rejected until a v6 list is added.

## mDNS

Each service-owning crate declares its advertisement at the call site
that owns the port:

```rust
ubos_mdns::declare_service!(
    pub REST_API_SERVICE = ubos_mdns::ServiceEntry {
        service: "_ubos",
        port: ubos_core::consts::Api::PORT,
        txt_keys:   &["path"],
        txt_values: &["/api/"],
    }
);
```

Firmware composes a `&'static [&'static ServiceEntry]` slice and passes
it to `ubos_mdns::run_mdns`. Responder code in
`ubos-mdns/src/responder.rs` runtime-appends `board=` and `mcu=` TXT
pairs per service. No `linkme` is used.

Today the workspace advertises:

| Service        | Port | Declared in                        |
|----------------|------|------------------------------------|
| `_http`        | 80   | `ubos-webui/src/lib.rs:24`         |
| `_ubos`        | 8080 | `ubos-api/src/lib.rs:12`           |
| `_prometheus`  | 8081 | `ubos-metrics/src/lib.rs:15`       |

Hostnames are composed per-board in each firmware's `config.rs` from
the `MDNS_HOSTNAME_BASE` const and the last MAC byte.

## Static assets

| Asset            | Source                       | Encoding | Cache                       |
|------------------|------------------------------|----------|-----------------------------|
| Dashboard SPA    | `ubos-webui/src/index.html`  | gzip     | `Cache-Control: no-store`   |
| Login form       | `ubos-webui/src/login.html`  | gzip     | `Cache-Control: no-store`   |
| Logo wordmark    | `ubos-webui/src/logo.svg`    | raw      | `public, max-age=86400`     |

HTML is gzipped at build time by `ubos-webui/build.rs` and embedded
via `include_bytes!`; no runtime compression cost.

## Limits and timeouts

| Constant                          | Value      | Location                              |
|-----------------------------------|------------|---------------------------------------|
| `Net::SOCKET_TIMEOUT`             | 10 s       | `ubos-core/src/consts.rs:10`          |
| `Http::PORT`                      | 80         | `ubos-core/src/consts.rs:16`          |
| `Http::RX_BUFFER_SIZE`            | 2048 B     | `ubos-core/src/consts.rs:20`          |
| `Http::TX_BUFFER_SIZE`            | 2048 B     | `ubos-core/src/consts.rs:21`          |
| `Http::REQUEST_BUFFER_SIZE`       | 2048 B     | `ubos-core/src/consts.rs:25`          |
| `Api::PORT`                       | 8080       | `ubos-core/src/consts.rs:35`          |
| `Api::RX_BUFFER_SIZE`             | 2048 B     | `ubos-core/src/consts.rs:36`          |
| `Api::TX_BUFFER_SIZE`             | 2048 B     | `ubos-core/src/consts.rs:37`          |
| `Api::REQUEST_BUFFER_SIZE`        | 2048 B     | `ubos-core/src/consts.rs:38`          |
| `Metrics::PORT`                   | 8081       | `ubos-core/src/consts.rs:92`          |
| `Metrics::RESPONSE_BUFFER_SIZE`   | 2048 B     | `ubos-core/src/consts.rs:95`          |
| `Metrics::MAX_METRICS`            | 64         | `ubos-core/src/consts.rs:107`         |
| `MAX_SESSIONS`                    | 8          | `ubos-auth/src/session.rs:22`         |
| `SESSION_TTL_SECS`                | 86 400     | `ubos-auth/src/session.rs:26`         |
| `MAX_TOKENS` (CSRF)               | 16         | `ubos-auth/src/csrf.rs:32`            |
| `CSRF_TTL_SECS`                   | 3 600      | `ubos-auth/src/csrf.rs:35`            |
| TCP pool size (both servers)      | 1          | `ubos-http/src/serve.rs`              |

Each server accepts one connection at a time. Concurrent scrapers or
browser tabs serialize on the accept loop; this matches the firmware's
single-threaded async model and keeps per-connection scratch off the
stack budget multiplier.

## Out of scope

No TLS on either server. No persistent credentials. No OTA, WiFi, or
NVS. No log streaming. Time-series ring buffer behind
`/api/system/statistics` is wired but not yet populated.
