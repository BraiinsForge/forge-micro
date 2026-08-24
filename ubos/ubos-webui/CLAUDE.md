# ubos-webui

Preact + Rsbuild frontend for the miner's port-80 web UI. `build.rs`
runs the Rsbuild build, Brotli-compresses the two single-file pages, and
`src/lib.rs` embeds them with `include_bytes!` for the firmware to serve.
`README.md` covers the dev loops in depth; this file is the working
contract — read it before touching `web/`.

## Size is the first constraint

**Every byte of the built bundle ships in firmware flash.** The pages
are Brotli-compressed at quality 11 and baked into the image via
`include_bytes!` (`src/lib.rs`), so they compete with firmware for a
fixed flash budget (OTA image cap is ~950 KiB). The number that matters
is the **Brotli size of `dist/*.html`**, not the raw or minified size.

Current baseline (`npm run build`, then `brotli -q 11`):

| Page | raw | brotli (embedded) |
|------|-----|-------------------|
| `dist/index.html` | ~75 KB | **~22.4 KB** |
| `dist/login.html` | ~4.6 KB | **~1.6 KB** |

Before/after any change that could move the needle, measure the delta:

```sh
npm run build
brotli -q 11 -c dist/index.html | wc -c    # bytes the firmware embeds
```

Guard the size:
- No new runtime dependencies. Preact + `preact/hooks` is the whole
  framework budget; it is pinned (see below). Adding a library is a
  reviewed decision, not a convenience.
- `web/icons.svg` is imported `?raw` (`app.tsx`) and shipped **byte for
  byte** — keep it comment-free and minimal; every character lands in
  every page.
- The login page is deliberately **framework-free** (`web/login.ts` is
  plain DOM, no Preact) so `login.html` stays under 2 KB. Do not import
  Preact or shared components into it.

## Component vs. inline: how to decide (size-wise)

The bundle is SWC-minified then Brotli-compressed with a large
window, so **Brotli already collapses repeated inline markup** — the
second copy of a tag tree costs only a back-reference, not its full
bytes. A Preact component adds a fixed cost that never compresses away:
the function definition plus a props object at every call site.

Rule of thumb:
- **1–2 uses of pure markup → inline it.** Extracting rarely wins on
  size; you pay the definition overhead for repetition Brotli was
  already deduping cheaply.
- **~3+ uses, or the block owns state/logic (`useState`, handlers,
  validation) → extract a component.** Here the per-call `h(Comp, …)`
  is smaller than the repeated inline tree and the definition amortizes,
  and you gain reuse and testability. `Field.tsx`, `Section.tsx`,
  `Chart.tsx` are the model.

When it is genuinely close, **measure, don't guess** — this is a
"verify, don't trust" call. A/B the Brotli byte count:

```sh
npm run build && brotli -q 11 -c dist/index.html | wc -c   # variant A
# apply the other variant
npm run build && brotli -q 11 -c dist/index.html | wc -c   # variant B
```

Readability and state encapsulation can justify a component even when it
costs a few bytes; shaving bytes alone rarely justifies one.

## Page layout and spacing

Type sizes are five tokens in `common.css` `:root`; every `font-size`
resolves to one so the pages line up and tune from one place. Reuse a
token, never hardcode a size.

| Token | Size | Used for |
|-------|------|----------|
| `--fs-label` | 12px | uppercase captions, field labels |
| `--fs-body` | 14px | body text, input text |
| `--fs-val` | 16px | metric values |
| `--fs-head` | 20px | section headings (`.psec h3`) |
| `--fs-title` | 24px | page titles (`.cfgTitle`, `.evTitle`) |

Page titles carry `--fw-emp` weight.

### Configuration-page rhythm

A config tab is a `<h2 class="cfgTitle">` over a `<form>` of stacked
groups. The vertical rhythm is fixed by three rules — match it, do not
add ad-hoc margins:

- **Title → first group: 24px.** `.cfgTitle` is `margin: 0 0 24px`.
- **Between groups and the Save button: 24px.** `.cfgsec form` is a
  flex column with `gap: 24px`.
- **Inside a group: 24px** from the 20px `h3` to its rows (`.psec`).
  A group marked `sep` (`<Section title sep>`) tightens that to **16px**
  and adds **24px** above itself, so it reads as a sub-group.
- **Field labels: 12px**, muted, `margin: 10px 0 2px`.

The first group of fields sits bare in a `<div class="fldRows">` with no
heading; every later group is a `<Section title sep>`. `ConfigThermal.tsx`
is the reference structure.

## Architecture

Two independent Rsbuild HTML entries, each inlined into one
self-contained page by `output.inlineScripts` + `output.inlineStyles`
(a single build pass emits both, so production is a bare `rsbuild
build`):

- `index.html` → `web/main.tsx` → `App` — the dashboard SPA (Preact).
- `login.html` → `web/login.ts` — a static form, no framework.

### Routing — hash-based, no router library

View state is the URL hash; there is no router dependency.
- Top-level view: `viewOf(hash)` in `web/app.tsx` maps `#config[/…]` →
  config, `#events` → events, everything else → dashboard. `App`
  subscribes to `hashchange` and re-renders.
- Config sub-tabs: `subOf(hash)` in `web/components/Config.tsx` matches
  `#config/<key>` against the `TABS` table.

Add a **top-level view**: extend `viewOf` (`app.tsx`) with the new hash,
add a `TopNav` link, and render on the match — no router. Add a **config
tab**: append one entry to `TABS` (`Config.tsx`) with `key`, `label`, and
`render`; `subOf` and the nav build themselves from the table. A new page
follows the config-page rhythm above and its own component; measure the
Brotli delta (`## Size is the first constraint`) before committing.

### Fetching — one poll, one signal

- `web/lib/store.ts` runs a **single 2 s poll** of `/api/info`
  (`startPoll`, `setInterval`), keeps 150-sample rolling buffers
  (`SAMPLES` = 150 × 2 s = 5 min) for the charts, and marks the link
  dead after 3 consecutive misses. The whole page reads from this one
  poll — do not add per-component polling.
- Shared reactive state uses the hand-rolled `createSignal`
  (`web/lib/signal.ts`): mutate the module-level object, then `notify()`.
  Components read via the `useValue` hook (`useDash`, `useToasts`). This
  replaces context/state libraries — follow the `store.ts` / `toast.ts`
  pattern for new global state.

### API client — `web/lib/api.ts`

The board splits its surface across two ports; the client hides it:
- JSON API on **:8080**, page/session/CSRF on **:80**. In prod
  `API = location.hostname:8080`; in dev `API = ''` so same-origin
  relative paths hit the dev mock/proxy.
- `getJSON(path)` — GET, throws on non-ok. Helpers: `getInfo`,
  `getConfig`.
- `csrfToken()` — `HEAD /` on the page origin returns a **single-use**
  `X-CSRF-Token`.
- `postJSON(path, body)` — fetches a fresh CSRF token, POSTs, and
  **retries** on the shallow worker pool (`RETRY_DELAYS = [0,250,500,
  1000]`), stopping on any 4xx. Surfaces the API's `{message}`.
- `postOnce(path, …)` — single-shot, no retry, for commands whose ack
  races the connection drop (restart, network switch, OTA chunks).

| Call | Endpoint | Port |
|------|----------|------|
| `getInfo` | `GET /api/info` (the 2 s poll) | 8080 |
| `getConfig` | `GET /api/config` | 8080 |
| bounds/events | `GET /api/system/{asic,thermal,cooling,network,events,events/catalog}` | 8080 |
| `postConfig` | `POST /api/config` (partial config) | 8080 |
| CSRF | `HEAD /` → `X-CSRF-Token` | 80 |
| session | `GET/POST /login`, `POST /logout` | 80 |

The firmware side of :80 (asset serving, login, CSRF issue/consume,
session cookie) is `src/dashboard.rs`; `/api/*` on :8080 lives in the
`ubos-api` crate, not here.

### Session expiry — redirect on 401

The session cookie can die while the SPA is loaded: it expires, is
invalidated, or is cleared when the board reboots (sessions are in-RAM).
`ubos-api` answers any `/api/*` from an anonymous request with a **401**
(CORS headers attached so the cross-origin fetch can read the status).
The SPA treats that as "session gone": `loginRedirect()` in
`web/lib/api.ts` does `location.replace('/login?expired=1')` — guarded so
concurrent 401s navigate once, `replace` so the dead SPA leaves no
history entry. `web/login.ts` reads `?expired=1` and shows "Your session
expired. Please sign in again." (distinct from the server's `?err=1`
bad-credentials banner).

Every API path funnels the check through one place: the `getJSON` /
`postJSON` / `postOnce` wrappers cover normal traffic, and the three
raw-`fetch` flows call `loginRedirect()` themselves — the restart poll
(`app.tsx`, which also unsticks the post-reboot "restarting" notice), the
OTA chunk uploader (`ConfigSystem.tsx`), and the support-snapshot fetch
(`snapshot.js`). **Any new raw `fetch` to `/api/*` must add the same
`if (response.status === 401) loginRedirect()` guard** — prefer routing
through the `api.ts` wrappers so you get it for free.

## Running the backend with mock values

No board required — leave `VITE_API_BASE` unset and `dev/mock.ts` answers
`/api/*` and the CSRF `HEAD /` from in-memory fixtures with light jitter
so the charts animate:

```sh
npm run dev            # http://localhost:5173  (dashboard at /, login at /login)
```

Against a live board instead:

```sh
VITE_API_BASE=http://<board-ip>:8080 npm run dev   # default creds root/root
```

The mock is imported only by `rsbuild.config.ts`, never by the app, so
it never reaches a bundle. **When you add a new API call, add a matching
fixture to the `switch` in `dev/mock.ts`** or it 404s in mock mode.
`README.md` documents the live-board proxy/CSRF details and the
`/login`-prefix caveat.

The mock mirrors the board's session flow: `POST /login` (the Sign in
button, any credentials) 302-redirects to `/`, `POST /logout` returns to
`/login`. It starts armed, so `/` loads without logging in. To exercise
the SPA's **session-expiry redirect** without a board, open
`http://localhost:5173/api/__mock/expire` in another tab while the
dashboard is up — the mock then answers every `/api/*` with 401, the
next poll bounces to `/login?expired=1`, and Sign in re-arms it.

## Build & verify

```sh
npm ci                 # first time only
npm run dev            # mock dev server (above)
npm run typecheck      # tsc --noEmit, strict; gate before any commit
npm run build          # production: dist/index.html + dist/login.html
```

`cargo build` re-runs `build.rs`, which shells out to `npm` (running
`npm ci` if `node_modules` is missing) and re-embeds the pages. It needs
the Rust toolchain from the `arm` shell:

```sh
nix develop .#arm -c bash -c 'cd ubos/ubos-webui && cargo build'
```

Run every frontend command from `ubos/ubos-webui/` — invoking
Rsbuild from the repo root ignores the local install and 404s every
route.

## Conventions

- Preact JSX: `class` (not `className`), automatic JSX runtime
  (`jsxImportSource: 'preact'`, set up by `@rsbuild/plugin-preact` in
  `rsbuild.config.ts`) — no `import { h }` needed.
- TypeScript, `strict` (`tsconfig.json`). `web/lib/types.ts` is the
  single home for the API domain types (`Info`, `Config`, `NetChange`,
  …) — extend it rather than re-declaring a shape locally. Type-only
  imports use `import type` (`verbatimModuleSyntax` enforces it). The
  shared leaf components export their props interface (`FieldProps`,
  `SectionProps`, …). Prefer `?.`/`??`/narrowing over `as`; a cast
  carries a one-line why. Every guard added for the type checker also
  ships as runtime bytes, so keep them to what strict actually needs.
- `preact` is **pinned** in `package.json` (exact version, no caret);
  keep it pinned. Preact 11 is RC-only — do not adopt it here until it
  ships stable.
- Comments: proper sentences, explain *why*. Never comment `icons.svg`
  (`?raw` ships it verbatim).
- `web/lib/format.ts` holds the shared value formatters (`fmtHr`,
  `fmtUp`, `fmtPower`, `niceStep`, …); reuse them rather than
  re-deriving display strings.
- The crate is gated in CI hermetically: `nix/webui.nix` builds the two
  pages under `buildNpmPackage`, whose `preBuild` runs `npm run
  typecheck` before `npm run build`, and `clippy-ubos-webui` /
  `test-ubos-webui` point the crate's `build.rs` at that prebuilt dist
  through `UBOS_WEBUI_DIST` so the sandbox needs no `npm`.
