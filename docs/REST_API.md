# REST API Reference

Status: implemented — the code wins. When this file and the code
disagree, the code is right and this file needs a patch.

Documents the HTTP surface served by the firmware. Targets two audiences:
operators driving the on-device SPA and machine clients (e.g. Braiins
Deck) speaking REST. The schema is ours, not reference-device's; the shape mirrors
reference-miner where compatibility costs nothing.

## Servers

| Port | Crate          | Purpose                                                |
|------|----------------|--------------------------------------------------------|
| 80   | `ubos-webui`   | dashboard SPA, login, logout, logo                     |
| 80   | `ubos-api`     | JSON REST API (`/api/*`)                               |
| 8081 | `ubos-metrics` | Prometheus text-format exposition at `/metrics`        |

Port 80 carries both surfaces behind one router. A path that starts with
`/api/` reaches `ubos-api`. Every other path reaches `ubos-webui`.

Discoverable via mDNS: `_http._tcp.local.` (port 80),
`_ubos._tcp.local.` (port 80), `_prometheus._tcp.local.` (port 8081).
Two service types name one port. This is valid mDNS.

CAUTION: The firmware no longer binds port 8080. A machine client that
holds port 8080 in its configuration must move to port 80. A client that
finds the board over mDNS reads the port from the SRV record and needs no
change.

## Crate map

| Crate          | Role                                                                      |
|----------------|---------------------------------------------------------------------------|
| `ubos-http`    | TCP accept loop, per-connection state machine, `Service` trait, `PrefixRouter`, the port 80 listener constants; generic response helpers (`read_body`, `write_json_response`, `MAX_RESPONSE_HEADERS`) |
| `ubos-auth`    | shared HTTP middleware: allow-list, auth, sessions, CSRF, CSPRNG          |
| `ubos-api`     | REST handlers, `REST_API_SERVICE` mDNS entry, schema types                |
| `ubos-webui`   | Zopfli-gzipped `index.html` / `login.html` and `logo.svg`, with `icon.png` inlined into each page; dashboard handler; `WEBUI_SERVICE` mDNS entry |
| `ubos-mdns`    | DNS-SD responder; per-service registration via `declare_service!`         |
| `ubos-metrics` | Prometheus exporter task and metric storage                               |

Each listener is served by a fixed pool of accept-loop workers spawned
through `ubos_http::http_worker_task!` (see `docs/HTTP_LISTENERS.md`).
Port 80 serves a `ubos_http::PrefixRouter` over
`ubos_api::RestApiService` (`ubos-api/src/rest.rs`), constructed with
`RestApiService::new`, and `ubos_webui::DashboardService`
(`ubos-webui/src/dashboard.rs`). Each handler seeds its credentials at
boot through `ubos_api::seed_api_credentials` and
`ubos_webui::seed_webui_credentials`. Port 8081 serves
`ubos_metrics::PrometheusMetricsService` (`ubos-metrics/src/metrics.rs`);
a separate `ubos_metrics::run_ingest` task drains the push channel into
the shared metric map the service renders.

## Request lifecycle

The router reads the request path first. A path that starts with `/api/`
reaches the REST handler. Every other path reaches the dashboard
handler. The two route tables do not overlap. Each handler then runs its
own chain, and the two chains differ only where this section says so.

### Dashboard handler

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
   clients are exempt. Anonymous `POST /login` (no active session
   cookie) is exempt. Failure: `403`,
   `{"message":"CSRF token invalid or missing"}`.
4. **Routing**. Known paths: `/`, `/login`, `/logout`, `/logo.svg`.
   Wrong method on a known path → `405`. Unknown path → `404`.

### REST handler

1. **IP allow-list** (`ubos-auth/src/allow_list.rs`). Same check as the
   dashboard handler.
2. **Authentication** (`ubos_auth::auth::authenticate`,
   `ubos-auth/src/auth.rs`). Every route under `/api/` requires auth.
   There are no anonymous endpoints. Anonymous requests receive `401`
   with `WWW-Authenticate: Basic realm="ubos"`.
3. **CSRF** (session-authenticated, state-changing only). `POST`,
   `PATCH`, `DELETE` from a session client must carry an `X-CSRF-Token`
   header matching a token bound to that session. Basic clients are
   exempt. Failure: `403`,
   `{"message":"CSRF token invalid or missing"}`.
4. **Routing**. `/api/system/*` is dispatched first (`system::dispatch`),
   then `/api/info` and `/api/config`. Wrong method on a known path →
   `405`. Unknown path → `404`.

Both handlers share the credential store, the session table and the CSRF
table, so a session started on the login form authenticates an API call
from the same page.

## Endpoints

`Auth` column: **none** = anonymous, **any** = Basic or session,
**+CSRF** = session also needs `X-CSRF-Token`.

### Pages

| Method | Path           | Auth        | Description                                                       |
|--------|----------------|-------------|-------------------------------------------------------------------|
| GET    | `/`            | session     | Dashboard SPA; gzipped; issues fresh `X-CSRF-Token` header |
| GET    | `/login`       | none        | Login form; gzipped                                          |
| POST   | `/login`       | none        | Form `username=&password=`; sets `session=` cookie on success     |
| POST   | `/logout`      | session     | Invalidates session, clears cookie                                |
| GET    | `/logo.svg`    | none        | The brand wordmark; `Cache-Control: public, max-age=86400`        |

Anonymous `GET /` is rewritten to `302 Found → /login`.

### REST API

| Method | Path           | Auth        | Description                                                       |
|--------|----------------|-------------|-------------------------------------------------------------------|
| GET    | `/api/info`    | any         | Telemetry blob consumed by the SPA poll, including a `chains` array with one entry per hashboard chain (`chain`, `state`, `chips`, `frequency_khz`, `hashrate`, `hottest_c`, and `pool`, the slot whose work the chain was last given, or `null` when it has none) |
| GET    | `/api/config`  | any         | `ConfigUpdate` snapshot: `pool`, `thermal`, `cooling` (`mode`, `target_temp_c`, `fixed_fan_pwm`, `min_fan_pwm`), `miner`, `api` (password blanked), `network` |
| POST   | `/api/config`  | any +CSRF   | Partial `ConfigUpdate` (any subset of those fields); returns `{"ok":true}` |

`/api/info` keeps PSU and chip-derived voltage separate.
`psu_voltage_measured_mv` is the physical PSU output reading;
`chain_voltage_mv` is the sum of core voltages reported by every
chip, or zero until the full chain has reported. The dashboard uses the
chain value only when the PSU reading is unavailable.

### `/api/system/*`

| Method | Path                              | Auth      | Status | Description                                                              |
|--------|-----------------------------------|-----------|--------|--------------------------------------------------------------------------|
| GET    | `/api/system/info`                | any       | 200    | `SystemInfo`; full telemetry with `apiVariant: "ubos-0.2"`, plus `hwInfo` (`psuModel` detected at runtime, `null` where the supply is not identifiable; `bootloaderVersion`/`fanControllerVersion` populated only on platforms that report them, `null` elsewhere; `hashboardIds` per-chain hashboard EEPROM serials, `null` slot when a chain's board is absent or unreadable; `chainsPresent` count of chains currently enumerated `Present`) |
| GET    | `/api/system/asic`                | any       | 200    | `MiningInfo`; chip facts (`asicModel`, `deviceModel`, `swarmColor`, `asicCount`), tuning defaults (`defaultFrequencyKhz`, `defaultVoltageMv`, `defaultEfficiencyMjPerTh`, and `defaultTuning`, the tuning a new config starts on: `"Fixed"` or `{ Power: { inputPowerMw } }`), `bounds: { frequencyKhz: { start, end }, voltageMv: { start, end }, tuners }`, where `tuners` lists the tuners the machine offers, each `{ Fixed: { name } }` or `{ Power: { name, readsEfficiency } }` |
| GET    | `/api/system/thermal`             | any       | 200    | `ThermalInfo`; accepted ranges as `bounds: { sampleIntervalMs, dangerousEstimatedTempC, dangerousChipTempC, cooldownTempC }` |
| GET    | `/api/system/cooling`             | any       | 200    | `CoolingInfo`; accepted ranges as `bounds: { targetTempC, minFanPwm, fixedFanPwm }` |
| GET    | `/api/system/statistics`          | any       | 200    | `SystemStatistics`; label set + (currently empty) ring of samples        |
| GET    | `/api/system/scoreboard`          | any       | 200    | `SystemScoreboardEntry[]`; currently empty                               |
| GET    | `/api/system/network`             | any       | 200    | `SystemNetwork`; current mode, pending-change flag, confirm window seconds |
| POST   | `/api/system/network`             | any +CSRF | 200    | `{"ok":true}`; validates and previews the change live (confirm-or-revert). 400 on parse or validation error |
| POST   | `/api/system/network/confirm`     | any +CSRF | 200    | Confirms a pending preview so it is no longer reverted; returns `{"message":"ack"}` |
| POST   | `/api/system/network/revert`      | any +CSRF | 200    | Reverts a pending preview immediately, restoring the prior config; returns `{"message":"ack"}` |
| GET    | `/api/system/events`              | any       | 200    | `{"events":[...]}`; oldest-first ring snapshot streamed close-delimited. Each record is `{seq, ts, sev, code, args}` where `code` is an FNV-1a slug hash and `args` a raw hex blob. `?since=<seq>` returns only records with `seq` greater than the cursor for incremental polling |
| GET    | `/api/system/events/catalog`      | any       | 200    | Event catalog as a JSON array of `{code, sev, fmt, args}` so clients decode records locally from `code` + `args` widths; `[]` until firmware installs catalogs |
| GET    | `/api/system/wifi/scan`           | any       | 501    | `{"message":"unsupported on this platform"}`                             |
| POST   | `/api/system/OTA`                 | any       | 501    | `{"message":"unsupported on this platform"}`                             |
| POST   | `/api/system/OTAWWW`              | any       | 501    | `{"message":"unsupported on this platform"}`                             |
| POST   | `/api/system/pause`               | any +CSRF | 200    | Stub: logs + sends `Manager::Stop`; returns `{"message":"ack"}`          |
| POST   | `/api/system/resume`              | any +CSRF | 200/409 | Logs + sends `Manager::Start`; returns `{"message":"ack"}`. 409 while a `dangerousTemperature` halt stands, with a `message` that states the halt |
| POST   | `/api/system/thermal/clear`       | any +CSRF | 200    | Clears a `dangerousTemperature` halt on the operator's word, then sends `Manager::Start`; returns `{"message":"ack"}` |
| POST   | `/api/system/restart`             | any +CSRF | 200    | Writes the ack, then calls `request_reboot` to reset the MCU once the socket drains |
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
`sample_interval_ms` and `fixed_fan_pwm`. Bodies are
serialized with `serde-json-core`. The `/api/system/*` handlers share
one 2 KiB stack scratch buffer (`SCRATCH_SIZE`,
`ubos-api/src/system.rs`). `/api/config` serializes into its own,
larger buffer (`CONFIG_RESPONSE_BUFFER_SIZE`, `ubos-api/src/rest.rs`),
sized from a measured worst-case body.

The old top-level `tuning` and `miner.algorithm` fields no longer apply.
The configuration schema hash changes, so firmware loads defaults instead of old stored configurations.

`miner.tuning` uses serde's externally tagged enum shape. A fixed target
is `{"Fixed":{"frequency_khz":525000,"voltage_mv":1400}}`; a power target
is `{"Power":{"input_power_mw":44000,"efficiency_mj_per_th":40000}}`.

A zero input-power target keeps the tuner at its safe boot point.
The efficiency is the input power for each TH/s of theoretical hashrate,
which is one hash for each core in each clock cycle.
The tuner stops its ramp at the frequency that gives this efficiency
at the target power. The API refuses a zero efficiency.

Each `pool` entry carries an optional `weight` (unsigned integer,
`0..=100000`) that sets its share of dispatched work in the ratio
scheduler. An absent key defaults to 0, which marks the slot
backup-only: it is mined solely when no weighted slot has work, so a
weightless pool list keeps the primary-plus-backups behavior. When
every entry is 0 the first slot is treated as weighted. Weights take
effect at runtime on the same config-watch publish that restarts the
pool sessions; the delivered split is observable via the
`board_pool_work_shares_total{pool=...}` metric.

The `cooling` section carries `mode`, either `"auto"` or `"fixed"`,
alongside `fixed_fan_pwm` and `min_fan_pwm`, both 0-100. Each duty is
validated on its own, independently of the mode, so one can be edited
while the other governs the fan. `target_temp_c` is validated under
both modes: it must stay below `thermal.dangerous_estimated_temp_c`,
the lower of the two halt thresholds, and that rule is what bounds how
low `dangerous_estimated_temp_c` can be set. A fixed duty of 0 is
accepted. The mining halt protects the board.

Under `"fixed"` the fan holds `fixed_fan_pwm`. No PID runs, so
`min_fan_pwm`, which bounds PID output, does not apply, and
`target_temp_c`, the PID setpoint, does not steer the fan. It steers the
fan again from the first thermal reading after the mode returns to
`"auto"`. Until that reading lands, the control tick raises the duty to
`min_fan_pwm` but never lowers it. A fixed duty below the floor does not
survive the switch. A duty above the floor is held until the PID takes
over. The cooling failsafes do not depend on the mode — a sensor fault,
a stalled thermal task and an idle board force the fan to full under
either. An idle board does so from its first idle tick, because it
publishes no temperature to steer by.

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

`API_VARIANT = "ubos-0.2"` (`ubos-api/src/schema/mod.rs:43`) ships on
every versioned response. Clients dispatch on this string rather than
treating ubos as a reference-device; the minor bumps on each schema-breaking
change while pre-1.0.

## Authentication

Two paths share a single credential store
(`pub static CREDENTIALS` at `ubos-auth/src/auth.rs:113`, seeded from
the persisted `config.api` section at boot via `set_credentials` and
refreshed on each config watch publish; `init_credentials` supplies the
`root` / `root` fallback only when the store is empty).

**HTTP Basic** — `Authorization: Basic <base64>`. Stateless,
re-validated per request, exempt from CSRF. Use this for machine
clients. Credentials are compared in constant time.

**Session cookie** — `POST /login` with a form body issues a 32-char
base64url token in `Set-Cookie: session=<token>; HttpOnly;
SameSite=Lax; Path=/`. The token is looked up in a fixed
`FnvIndexMap<_, _, 8>` with 24 h TTL; the oldest entry is evicted on
overflow so a tab refresh cannot lock out parallel tabs. See
`ubos-auth/src/session.rs`.
Lax still sends the cookie on a cross-site top-level GET navigation,
and a cookie ignores the port. So every GET route on the dashboard and
on the API server must have no side effects.

Credentials are plaintext, not hashed. They are seeded from the
persisted `config.api` section at boot and refreshed whenever the
config watch publishes, so a `POST /api/config` change takes effect at
runtime and persists across reboots.

## CSRF

State-changing requests from session clients carry `X-CSRF-Token`.
The token is minted on `GET /` (single-use, bound to the issuing
session, 1 h TTL) and stored in `FnvIndexMap<_, _, 16>` at
`ubos-auth/src/csrf.rs`. Mismatched session id or expired token → `403`.

Basic auth bypasses the check; the `Authorization` header already
proves identity per request without any ambient browser credentials to
forge.

## CORS

The firmware sends no CORS headers on port 80 and answers no preflight.
The SPA and the API share one origin, so the browser applies no
cross-origin restriction and each API call costs one request.

CAUTION: Do not return a CORS header on port 80. The earlier design on
port 8080 echoed the request origin with
`Access-Control-Allow-Credentials: true`. Every page in the browser then
had read access to the API.

The session cookie was not what that echo reached, because
`dashboard.rs` sets it `SameSite=Lax`, which a cross-site fetch
never carries. It reached the `Basic`
credentials. The API answers a 401 with
`WWW-Authenticate: Basic realm="ubos"`, so the browser sends the
credentials it holds for that origin on its own. One origin needs no such
header.

The metrics listener on port 8081 is the one cross-origin surface. It
answers with a wildcard `Access-Control-Allow-Origin` and takes no
credentials.

The IP allow-list is the security boundary: only clients on the RFC 1918
LAN can reach the surface at all.

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
        port: ubos_http::consts::Http::PORT,
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
| `_http`        | 80   | `ubos-webui/src/lib.rs`            |
| `_ubos`        | 80   | `ubos-api/src/lib.rs`              |
| `_prometheus`  | 8081 | `ubos-metrics/src/lib.rs`          |

Hostnames are composed per-board in each firmware's `config.rs` from
the `MDNS_HOSTNAME_BASE` const and the last MAC byte.

## Static assets

| Asset            | Source                       | Encoding | Cache                       |
|------------------|------------------------------|----------|-----------------------------|
| Dashboard SPA    | `ubos-webui/index.html`      | gzip     | `Cache-Control: no-store`   |
| Login form       | `ubos-webui/login.html`      | gzip     | `Cache-Control: no-store`   |
| Logo wordmark    | `$UBOS_BRAND/logo.svg`       | gzip     | `public, max-age=86400`     |
| Tab icon         | `$UBOS_BRAND/icon.png`       | inlined  | rides inside each page      |

Assets are Zopfli-gzipped at build time by `ubos-webui/build.rs` and
embedded via `include_bytes!`; no runtime compression cost. The tab icon
is the exception. It has no path of its own: `build.rs` encodes it into
a `data:` URI that each page links, so a page load asks for the page and
the wordmark and nothing more. A board answers as few as two requests at
once, and a third request costs the page its wordmark.

## Limits and timeouts

| Constant                          | Value      | Location                              |
|-----------------------------------|------------|---------------------------------------|
| `Net::SOCKET_TIMEOUT`             | 10 s       | `ubos-core/src/consts.rs`             |
| `Http::PORT`                      | 80         | `ubos-http/src/consts.rs`             |
| `Http::RX_BUFFER_SIZE`            | 2048 B     | `ubos-http/src/consts.rs`             |
| `Http::TX_BUFFER_SIZE`            | 2048 B     | `ubos-http/src/consts.rs`             |
| `Http::REQUEST_BUFFER_SIZE`       | 2048 B     | `ubos-http/src/consts.rs`             |
| `Http::SOCKET_TIMEOUT`            | 10 s       | `ubos-http/src/consts.rs`             |
| `Http::IDLE_TIMEOUT`              | 3 s        | `ubos-http/src/consts.rs`             |
| `Metrics::PORT`                   | 8081       | `ubos-metrics/src/consts.rs`          |
| `MAX_SESSIONS`                    | 8          | `ubos-auth/src/session.rs`            |
| `SESSION_TTL_SECS`                | 86 400     | `ubos-auth/src/session.rs`            |
| `MAX_TOKENS` (CSRF)               | 16         | `ubos-auth/src/csrf.rs`               |
| `CSRF_TTL_SECS`                   | 3 600      | `ubos-auth/src/csrf.rs`               |
| `http_pool`                       | 1 to 8     | `ubos-machine/src/control_board.rs`   |
| `metrics_http_pool`               | 1 to 5     | `ubos-machine/src/control_board.rs`   |
| `MAX_METRICS`                     | 64         | each board's `src/config.rs`          |
| `METRICS_RESPONSE_BYTES`          | 4096 B     | each board's `src/config.rs`          |

Each listener is served by a fixed pool of accept-loop workers. Port 80
keeps a connection alive, so an open dashboard holds one slot for as
long as the page is open. See `docs/HTTP_LISTENERS.md` for the capacity
model, the per-board depths, and the RAM bound that caps them.

The last two rows are the exposition map and its render buffer. Each
board states them in its own `config.rs`. Every board in this tree
holds 64 series in 4096 bytes. See "Footprint" in `docs/STRUCTURE.md`.

## Out of scope

No TLS on either listener. No OTA, WiFi, or NVS. No log streaming.
Time-series ring buffer behind `/api/system/statistics` is wired but
not yet populated.
