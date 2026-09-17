import type { ClientRequest } from 'node:http';
import { defineConfig } from '@rsbuild/core';
import { pluginPreact } from '@rsbuild/plugin-preact';
import { mockApi } from './dev/mock';
import { boardCsrf } from './dev/board-proxy';

// Build config for both HTML entries. Rsbuild inlines each page's JS and
// CSS into one self-contained file (output.inlineScripts/inlineStyles),
// so `rsbuild build` emits dist/index.html and dist/login.html directly
// — build.rs then gzips those two and embeds them.

// Build provenance is not known here. A production bundle carries
// these two tokens and build.rs replaces them with the revision and
// the time when it embeds the pages. A bundle stamped at this step
// would change with every commit, and every build that embeds it
// would follow. The dev server embeds nothing and runs no build.rs,
// so its footer says `dev` rather than showing the tokens.
const provenance = (env: string) =>
  env === 'production'
    ? { rev: '@UBOS_WEBUI_BUILD_REV@', time: '@UBOS_WEBUI_BUILD_TIME@' }
    : { rev: 'dev', time: '0' };

// When VITE_API_BASE (e.g. http://192.168.1.50) is set, the dev server
// proxies to a live board; otherwise the mock middleware answers from
// fixtures so the UI runs with no board attached. The board serves the
// JSON API, the page, CSRF and login on one listener, so one origin
// covers everything.
const API_BASE = process.env.VITE_API_BASE;

// TEMPORARY: remove once the firmware raises its request-header limit.
//
// The board parses at most sixteen request headers and resets the
// connection on the seventeenth, which a current browser trips: a
// navigation POST to /login or a fetch to /api carries client-hint and
// fetch-metadata headers the firmware never reads. Dropping those before
// the proxy forwards the request keeps the header count under the limit.
// The board ignores every header removed here, so this changes nothing
// but the count.
//
// This only masks the limit for the dev proxy; a browser talking to the
// board directly still hits it. The real fix is in the firmware, and
// this whole block comes out when that lands.
const STRIP_HEADERS = [
  'sec-ch-ua',
  'sec-ch-ua-mobile',
  'sec-ch-ua-platform',
  'sec-fetch-site',
  'sec-fetch-mode',
  'sec-fetch-user',
  'sec-fetch-dest',
  'upgrade-insecure-requests',
  'cache-control',
  'pragma',
  'accept-language',
  'referer',
  'priority',
  'dnt',
];

// `cookieDomainRewrite: ''` strips the Domain attribute from the board's
// Set-Cookie so the session cookie is stored for localhost; that lets a
// login performed through the dev server authenticate the proxied /api
// and CSRF requests that follow.
const proxyToBoard = (target: string) => ({
  target,
  changeOrigin: true,
  cookieDomainRewrite: '',
  on: {
    proxyReq: (proxyReq: ClientRequest) => {
      for (const header of STRIP_HEADERS) proxyReq.removeHeader(header);
    },
  },
});

export default defineConfig(({ env }) => ({
  plugins: [pluginPreact()],
  source: {
    entry: {
      index: './web/main.tsx',
      login: './web/login.ts',
    },
    define: {
      __BUILD_REV__: JSON.stringify(provenance(env).rev),
      __BUILD_TIME__: JSON.stringify(provenance(env).time),
    },
  },
  html: {
    // Each entry keeps its own template (the pre-paint theme script and
    // page shell); Rsbuild injects the entry's inlined script into it.
    template: ({ entryName }) => (entryName === 'login' ? './login.html' : './index.html'),
    // Inject into <body>, after the app div. Browsers ignore `defer` on
    // inline scripts, so a head-injected inlined bundle runs before the
    // mount point exists and the render crashes.
    inject: 'body',
  },
  output: {
    // Inline everything so each page is a single self-contained file.
    inlineScripts: true,
    inlineStyles: true,
    // The pages are inlined, so hashed asset names buy nothing.
    filenameHash: false,
    // Three compress passes shave a little more off the minified bundle.
    minify: {
      jsOptions: {
        minimizerOptions: {
          compress: { passes: 3 },
          format: { comments: false },
        },
      },
    },
  },
  performance: {
    // Keep one bundle per page so nothing splits into an external chunk.
    chunkSplit: { strategy: 'all-in-one' },
  },
  tools: {
    rspack: (config) => {
      // web/icons.svg is imported `?raw` (app.tsx) and shipped verbatim;
      // asset/source yields the file contents as a string.
      config.module ??= {};
      config.module.rules ??= [];
      config.module.rules.push({ resourceQuery: /raw/, type: 'asset/source' });
    },
  },
  server: {
    // Keep the historical dev port so the documented URLs and the mock's
    // /api/__mock/expire instructions stay accurate.
    port: 5173,
    ...(API_BASE
      ? {
          proxy: {
            '/api': proxyToBoard(API_BASE),
            // Session endpoints. Proxied so a login through the dev
            // server yields a localhost-scoped session cookie.
            '/login': proxyToBoard(API_BASE),
            '/logout': proxyToBoard(API_BASE),
            // CSRF `HEAD /` is handled by the boardCsrf middleware, not
            // here, because `/` also serves the dev app.
          },
        }
      : {}),
    // `server.setup` runs before Rsbuild's own handlers, so the mock/CSRF
    // middleware registered here already precedes them.
    setup({ server }) {
      server.middlewares.use(API_BASE ? boardCsrf(API_BASE) : mockApi());
    },
  },
}));
