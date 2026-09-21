# ubos-webui

Preact + Rsbuild frontend for the miner's port-80 web UI. `build.rs` runs
the Rsbuild build, gzips the two resulting single-file pages
(`index.html`, `login.html`) plus the declared brand's `logo.svg` with
Zopfli, encodes that brand's `icon.png` into a `data:` URI inside each
page, and `src/lib.rs` embeds the results with `include_bytes!` for the
firmware to serve. Gzip rather than Brotli because browsers only
advertise `br` over HTTPS and the board serves plain HTTP.

The pages carry no brand of their own. `UBOS_BRAND` names a directory
that holds the name, the three colours, the wordmark and the tab icon,
and each board declares which one it ships. `../../brand/README.md` is the authority;
the section below covers what that means for the dev server.

```
index.html  login.html   HTML entries (Rsbuild roots)
web/                     app sources: app.tsx, components/, lib/, styles/
dev/mock.ts              dev-only fixture API (no board needed)
dev/board-proxy.ts       dev-only `HEAD /` CSRF forwarder for a live board
rsbuild.config.ts        build config: entries, single-file inlining, dev mock/proxy
tsconfig.json            strict TypeScript config; web/lib/types.ts holds the API types
src/                     Rust side: route table, asset serving, tests
```

## Toolchain

The frontend needs nothing but Node — Rsbuild wants `^20.19 || >=22.12`
— so the dev loop and the production build run with a plain system or
volta install. All commands below assume `ubos/ubos-webui` as the
working directory and `node_modules` already installed:

```sh
cd ubos/ubos-webui
npm ci                    # first time only
```

Run them from the crate directory. Invoking rsbuild from the repo root
ignores the local install and serves 404s for every route.

The `arm` shell carries `nodejs` for the same commands. Wrap any
command below when you want that Node instead of the system one:

```sh
nix develop .#arm -c bash -c 'cd ubos/ubos-webui && <command>'
```

`cargo build` does not use this Node: every dev shell sets
`UBOS_WEBUI_DIST`, for the reason the header of `nix/shells.nix`
gives. See "Production build" below.

## Types

The frontend is TypeScript in strict mode. `tsconfig.json` holds the
compiler options. `web/lib/types.ts` holds the API types that every
component shares.

```sh
npm run typecheck        # tsc --noEmit
```

`npm run typecheck` reports type errors and writes no output. `npm run
build` does not type check, so run the check yourself before a commit.
The nix build runs the same check before the bundle build, so a type
error there fails the build.

## Which brand the dev server shows

A production build emits a token for each brand value, and `build.rs`
replaces it with the brand the board ships. A dev build runs no
`build.rs`, so the dev server reads `brand.json` itself and shows the
real values.

The `dev` script sets `UBOS_BRAND` to `../../brand/ubos`, because
cargo declares it for every build it drives and the dev server is not
one of them. Export the variable to see the other brand:

```sh
UBOS_BRAND=../../brand/forge npm run dev
```

The dev server serves that directory's `logo.svg` at `/logo.svg`, which
is the same file the firmware serves, and inlines its `icon.png` the
same way `build.rs` does.

## Dev loop against a live board

```sh
VITE_API_BASE=http://192.0.2.111 npm run dev
```

Serves on 5173; `npm run dev -- --port 5199` moves it.

Then open <http://localhost:5173/login> and sign in (default
credentials are `root` / `root`). The dashboard is at
<http://localhost:5173/>. Visiting it without a session — or once the
session expires or the board reboots — makes the first `/api` call return
401, and the SPA redirects to `/login?expired=1` (the login page then
shows a "session expired" notice). See "Session expiry" below.

`VITE_API_BASE` is the board's origin. The board serves its whole
surface on one listener, so every dev route proxies to the same place:

| Dev path | Proxied to | Purpose |
|----------|------------|---------|
| `/api/*` | `192.0.2.111` | JSON API |
| `/login`, `/logout` | `192.0.2.111` | session endpoints |
| `HEAD /` | `192.0.2.111` | CSRF token, via `dev/board-proxy.ts` |

`HEAD /` cannot be a plain proxy entry because `/` also serves the dev
app, so `boardCsrf` middleware special-cases it and copies the
`X-CSRF-Token` response header back. `cookieDomainRewrite: ''` strips
the `Domain` attribute from the board's `Set-Cookie`, so the session
cookie is stored for `localhost` and authenticates the proxied `/api`
and CSRF requests that follow.

Verified working against 192.0.2.111: form POST to `/login` returns
302 with a localhost-scoped `session` cookie, `GET /api/info` returns
200, and `HEAD /` yields a fresh `X-CSRF-Token`.

Caveat: because the proxy matches the `/login` prefix, the login *page*
is served by the board (its own built copy) rather than from `web/`.
Edits to `login.html` / `web/login.ts` / `web/styles/login.css` are not
picked up in this mode — use mock mode for those.

## Dev loop with no board

Leave `VITE_API_BASE` unset and `dev/mock.ts` answers `/api/*` and the
CSRF `HEAD /` from in-memory fixtures with light jitter, so the charts
animate:

```sh
npm run dev
```

Both pages come from source here: `/` for the dashboard, `/login` (or
`/login.html`) for the login page. The mock middleware is imported only
by `rsbuild.config.ts`, never by the app, so it never reaches a bundle.

The mock mirrors the board's session flow so the login and logout
controls behave as they do on hardware: `POST /login` accepts any
credentials and redirects to `/`, `POST /logout` redirects back to
`/login`. It starts authenticated, so opening `/` works without signing
in first.

## Session expiry

The session cookie can die while the SPA is loaded — it expires, is
invalidated, or is cleared when the board reboots (sessions are in-RAM).
The board answers any `/api/*` from an unauthenticated request with a
`401`, and the SPA treats that as "session gone": it redirects to
`/login?expired=1`, which shows "Your session expired. Please sign in
again." This is wired into every API path — the `getJSON`/`postJSON`/
`postOnce` client wrappers plus the raw-`fetch` flows (restart poll, OTA
upload, support snapshot). See `web/lib/api.ts` (`loginRedirect`).

To exercise this without a board, run the mock (`npm run dev`), open the
dashboard, then visit <http://localhost:5173/api/__mock/expire> in a
second tab. The mock then answers every `/api/*` with `401` just like the
board; the dashboard's next 2 s poll catches it and bounces to
`/login?expired=1`. Signing in again re-arms the mock.

## Production build

```sh
npm run build
```

`rsbuild build` writes `dist/index.html` and `dist/login.html` in a
single pass. `output.inlineScripts` and `output.inlineStyles` fold all
JS and CSS into each page, and the SWC minifier shrinks the bundle.

`build.rs` does this automatically: if `UBOS_WEBUI_DIST` is set
it reads the two pages from that directory, otherwise it runs `npm ci`
(when `node_modules` is missing) and `npm run build` itself. The doc
comment of `build.rs` says which build takes which path. Nix reads the
tracked tree, so a new frontend file reaches a shell build only after
`git add` and a new `nix develop`.

The footer's revision and time are not in the bundle. `source.define`
puts two fixed tokens there, and `build.rs` replaces them with
`UBOS_WEBUI_BUILD_REV` and `UBOS_WEBUI_BUILD_TIME` when it embeds the
pages. So the same dist serves every commit and nix caches it, while
each firmware still shows the revision that built it. The image builds
set both variables. The dev shells and the flake checks set neither,
and there `build.rs` reads `git rev-parse` and the system clock when
it runs. A dev shell that set them from the flake gave a new time on
every run of a dirty tree, and that rebuilt this crate and every
firmware behind it on every run. With nothing set, a tree with no
change builds nothing and keeps the stamp of the last build.

## Nix status

`nix/webui.nix` is the hermetic `buildNpmPackage` that lets the Rust
checks build without Node: `flake.nix` builds it and threads the
output through `nix/lib.nix` as `webuiDistEnv`, which
`clippy-ubos-webui` and `test-ubos-webui` (`nix/checks.nix`) and the
dev shells (`nix/shells.nix`) set as `UBOS_WEBUI_DIST`. The derivation's `preBuild` runs `npm run typecheck`, so the
type check gates the build. The file's header documents the one-time
`prefetch-npm-deps` call for when `package-lock.json` moves.
