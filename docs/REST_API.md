# REST API Reference

Documents the HTTP surface served by the firmware. Targets two audiences:
operators driving the on-device SPA and machine clients (e.g. Braiins
Deck) speaking REST. The schema is ours, not reference-device's; the shape mirrors
reference-miner where compatibility costs nothing.

## Servers

| Port | Crate          | Purpose                                                |
|------|----------------|--------------------------------------------------------|
| 80   | `ubos-api`     | dashboard SPA, login, REST API                         |
| 8081 | `ubos-metrics` | Prometheus text-format exposition at `/metrics`        |

Both are discoverable via mDNS (`_http._tcp.local.`,
`_prometheus._tcp.local.`).

## Crate map

| Crate         | Role                                                                  |
|---------------|-----------------------------------------------------------------------|
| `ubos-http`   | TCP accept loop, per-connection state machine, `Service` trait        |
| `ubos-api`    | request routing, auth, CSRF, allow-list, REST handlers, schema types  |
| `ubos-webui`  | gzipped `index.html` / `login.html` and raw `logo.svg`                |
| `ubos-mdns`   | DNS-SD responder; per-service registration via `declare_service!`     |
| `ubos-metrics`| Prometheus exporter task and metric storage                           |

`ubos-api::run` (`ubos-api/src/lib.rs:38`, re-exported from
`dashboard::run`) is the single firmware entry point for port 80;
`ubos-metrics::run` is the entry point for port 8081.

## Request lifecycle

Every request on port 80 passes through, in order:

1. **IP allow-list** (`ubos-api/src/dashboard.rs:446`). Off-list peers
   get `401` with body `{"message":"Client not in allowed network range"}`
   verbatim, before any auth attempt.
2. **Authentication**. Anonymous routes (`/login`, `/logo.svg`) skip.
   Everything else requires either `Authorization: Basic …` or a valid
   `session=` cookie. Failure: `401`, `WWW-Authenticate: Basic realm="ubos"`.
3. **CSRF** (session-authenticated, state-changing only). `POST`,
   `PATCH`, `DELETE` from a session client must carry an `X-CSRF-Token`
   header matching a token bound to that session. Basic clients are
   exempt; `GET` and `POST /login` are exempt. Failure: `403`,
   `{"message":"csrf"}`.
4. **Routing**. `/api/system/*` is dispatched first (`system::dispatch`),
   then the legacy table (`/`, `/login`, `/logout`, `/logo.svg`,
   `/api/info`, `/api/config`). Wrong method on a known path → `405`.
   Unknown path → `404`.

## Endpoints

`Auth` column: **none** = anonymous, **any** = Basic or session,
**+CSRF** = session also needs `X-CSRF-Token`.

### Pages and legacy API (port 80)

| Method | Path           | Auth        | Description                                                       |
|--------|----------------|-------------|-------------------------------------------------------------------|
| GET    | `/`            | session     | Dashboard SPA; gzipped; issues fresh `X-CSRF-Token` header        |
| GET    | `/login`       | none        | Login form; gzipped                                               |
| POST   | `/login`       | none        | Form `username=&password=`; sets `session=` cookie on success     |
| POST   | `/logout`      | session     | Invalidates session, clears cookie                                |
| GET    | `/logo.svg`    | none        | Braiins wordmark; `Cache-Control: public, max-age=86400`          |
| GET    | `/api/info`    | any         | Legacy telemetry blob consumed by the SPA poll                    |
| GET    | `/api/config`  | any         | `{"pool": […]}` from `ubos-config`                                |
| POST   | `/api/config`  | any +CSRF   | Update pool list; returns `{"ok":true}`                           |

Anonymous `GET /` is rewritten to `302 Found → /login`.

### `/api/system/*` (port 80)

| Method | Path                              | Auth      | Status | Description                                                              |
|--------|-----------------------------------|-----------|--------|--------------------------------------------------------------------------|
| GET    | `/api/system/info`                | any       | 200    | `SystemInfo`; full telemetry with `apiVariant: "ubos-0.1"`               |
| GET    | `/api/system/asic`                | any       | 200    | `SystemAsic`; `asicModel: "BM1362"`, frequency/voltage options           |
| GET    | `/api/system/statistics`          | any       | 200    | `SystemStatistics`; label set + (currently empty) ring of samples        |
| GET    | `/api/system/scoreboard`          | any       | 200    | `SystemScoreboardEntry[]`; currently empty                               |
| GET    | `/api/system/logs`                | any       | 501    | `{"message":"not implemented"}`                                          |
| GET    | `/api/system/wifi/scan`           | any       | 501    | `{"message":"unsupported on this platform"}`                             |
| POST   | `/api/system/OTA`                 | any       | 501    | `{"message":"unsupported on this platform"}`                             |
| POST   | `/api/system/OTAWWW`              | any       | 501    | `{"message":"unsupported on this platform"}`                             |
| POST   | `/api/system/pause`               | any +CSRF | 200    | Stub: logs + sends `Manager::Stop`; returns `{"message":"ack"}`          |
| POST   | `/api/system/resume`              | any +CSRF | 200    | Stub: logs + sends `Manager::Start`; returns `{"message":"ack"}`         |
| POST   | `/api/system/restart`             | any +CSRF | 200    | Stub: logs only; returns `{"message":"ack"}`                             |
| POST   | `/api/system/identify`            | any +CSRF | 200    | Stub: logs only; returns `{"message":"ack"}`                             |
| POST   | `/api/system/blockFound/dismiss`  | any +CSRF | 200    | Stub: logs only; returns `{"message":"ack"}`                             |
| PATCH  | `/api/system`                     | any +CSRF | 200    | Stub: logs body length; returns `{"message":"ack"}`. Not persisted.      |

Stubs are wired to the router and return the documented response so
external clients can integrate against the full surface. Action handlers
live in `ubos-api/src/system.rs:585-599`; PATCH at
`ubos-api/src/system.rs:398`.

### Metrics (port 8081)

| Method | Path       | Description                                                |
|--------|------------|------------------------------------------------------------|
| GET    | `/metrics` | Prometheus text exposition (`text/plain; version=0.0.4`)   |

No auth, no allow-list. Any other path → `404`; any other method on
`/metrics` → `405 Allow: GET`.

## Schemas

Each response struct lives in its own module under
`ubos-api/src/schema/` and derives `serde::Serialize` with
`#[serde(rename_all = "camelCase")]`. Bodies are serialized with
`serde-json-core` into a 2 KiB stack scratch buffer
(`ubos-api/src/system.rs`).

| Struct                  | Endpoint(s)                                        |
|-------------------------|----------------------------------------------------|
| `SystemInfo`            | `GET /api/system/info`                             |
| `SystemAsic`            | `GET /api/system/asic`                             |
| `SystemStatistics`      | `GET /api/system/statistics`                       |
| `SystemScoreboardEntry` | `GET /api/system/scoreboard`                       |
| `GenericResponse`       | action stubs, 501 responses, errors                |

`API_VARIANT = "ubos-0.1"` (`ubos-api/src/schema/mod.rs:24`) ships on
every versioned response. Clients dispatch on this string rather than
treating ubos as a reference-device; the minor bumps on each schema-breaking
change while pre-1.0.

## Authentication

Two paths share a single credential store
(`pub static CREDENTIALS` at `ubos-api/src/auth.rs:112`, initialised
to `root` / `root` by `init_credentials`).

**HTTP Basic** — `Authorization: Basic <base64>`. Stateless,
re-validated per request, exempt from CSRF. Use this for machine
clients. Credentials are compared in constant time.

**Session cookie** — `POST /login` with a form body issues a 32-char
base64url token in `Set-Cookie: session=<token>; HttpOnly;
SameSite=Strict; Path=/`. The token is looked up in a fixed
`FnvIndexMap<_, _, 8>` with 24 h TTL; the oldest entry is evicted on
overflow so a tab refresh cannot lock out parallel tabs. See
`ubos-api/src/session.rs`.

Credentials are plaintext, not hashed, and not persisted across reboot.
`PATCH /api/system` is a stub; the defaults reapply on every boot.

## CSRF

State-changing requests from session clients carry `X-CSRF-Token`.
The token is minted on `GET /` (single-use, bound to the issuing
session, 1 h TTL) and stored in `FnvIndexMap<_, _, 16>` at
`ubos-api/src/csrf.rs`. Mismatched session id or expired token → `403`.

Basic auth bypasses the check; the `Authorization` header already
proves identity per request without any ambient browser credentials to
forge.

## IP allow-list

Network-level screen applied before auth (`ubos-api/src/allow_list.rs`).
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
    pub HTTP_API_SERVICE = ubos_mdns::ServiceEntry {
        service: "_http",
        port: 80,
        txt_keys:   &["path"],
        txt_values: &["/"],
    }
);
```

Firmware composes a `&'static [&'static ServiceEntry]` slice and passes
it to `ubos_mdns::run_mdns`. Responder code in
`ubos-mdns/src/responder.rs` runtime-appends `board=` and `mcu=` TXT
pairs per service. No `linkme` is used.

Today the workspace advertises:

| Service        | Port | Declared in                       |
|----------------|------|-----------------------------------|
| `_http`        | 80   | `ubos-api/src/lib.rs:19`          |
| `_prometheus`  | 8081 | `ubos-metrics/src/lib.rs`         |

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

| Constant                          | Value      | Location                          |
|-----------------------------------|------------|-----------------------------------|
| `Net::SOCKET_TIMEOUT`             | 10 s       | `ubos-core/src/consts.rs:10`      |
| `Http::PORT`                      | 80         | `ubos-core/src/consts.rs:16`      |
| `Http::RX_BUFFER_SIZE`            | 2048 B     | `ubos-core/src/consts.rs:20`      |
| `Http::TX_BUFFER_SIZE`            | 2048 B     | `ubos-core/src/consts.rs:21`      |
| `Http::REQUEST_BUFFER_SIZE`       | 2048 B     | `ubos-core/src/consts.rs:25`      |
| `Metrics::PORT`                   | 8081       | `ubos-core/src/consts.rs:117`     |
| `Metrics::RESPONSE_BUFFER_SIZE`   | 2048 B     | `ubos-core/src/consts.rs:120`     |
| `Metrics::MAX_METRICS`            | 64         | `ubos-core/src/consts.rs:132`     |
| `MAX_SESSIONS`                    | 8          | `ubos-api/src/session.rs:22`      |
| `SESSION_TTL_SECS`                | 86 400     | `ubos-api/src/session.rs:26`      |
| `MAX_TOKENS` (CSRF)               | 16         | `ubos-api/src/csrf.rs:32`         |
| `CSRF_TTL_SECS`                   | 3 600      | `ubos-api/src/csrf.rs:35`         |
| TCP pool size (both servers)      | 1          | `ubos-http/src/serve.rs`          |

Each server accepts one connection at a time. Concurrent scrapers or
browser tabs serialize on the accept loop; this matches the firmware's
single-threaded async model and keeps per-connection scratch off the
stack budget multiplier.

## Out of scope

No TLS on either server. No persistent credentials. No OTA, WiFi, or
NVS. No log streaming. Time-series ring buffer behind
`/api/system/statistics` is wired but not yet populated.
