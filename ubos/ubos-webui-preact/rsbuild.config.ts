import { defineConfig } from '@rsbuild/core';
import { pluginPreact } from '@rsbuild/plugin-preact';
import { mockApi } from './dev/mock';
import { boardCsrf } from './dev/board-proxy';

// Build config for both HTML entries. Rsbuild inlines each page's JS and
// CSS into one self-contained file (output.inlineScripts/inlineStyles),
// so `rsbuild build` emits dist/index.html and dist/login.html directly
// — build.rs then Brotli-compresses those two and embeds them.

const BUILD_REV = process.env.UBOS_WEBUI_BUILD_REV || 'dev';
const BUILD_TIME = process.env.UBOS_WEBUI_BUILD_TIME || '0';

// When VITE_API_BASE (e.g. http://192.168.1.50:8080) is set, the dev
// server proxies to a live board; otherwise the mock middleware answers
// from fixtures so the UI runs with no board attached.
const API_BASE = process.env.VITE_API_BASE;
// The board serves JSON on the API port and the page/CSRF/login on
// port 80. Derive the port-80 origin from the API base.
const BOARD_80 = API_BASE ? `${new URL(API_BASE).protocol}//${new URL(API_BASE).hostname}` : '';

// `cookieDomainRewrite: ''` strips the Domain attribute from the board's
// Set-Cookie so the session cookie is stored for localhost; that lets a
// login performed through the dev server authenticate the proxied /api
// and CSRF requests that follow.
const proxyToBoard = (target: string) => ({ target, changeOrigin: true, cookieDomainRewrite: '' });

export default defineConfig({
  plugins: [pluginPreact()],
  source: {
    entry: {
      index: './web/main.tsx',
      login: './web/login.ts',
    },
    define: {
      __BUILD_REV__: JSON.stringify(BUILD_REV),
      __BUILD_TIME__: JSON.stringify(BUILD_TIME),
    },
  },
  html: {
    // Each entry keeps its own template (the pre-paint theme script and
    // page shell); Rsbuild injects the entry's inlined script into it.
    template: ({ entryName }) => (entryName === 'login' ? './login.html' : './index.html'),
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
            // JSON API (port 8080).
            '/api': proxyToBoard(API_BASE),
            // Session endpoints (port 80). Proxied so a login through the
            // dev server yields a localhost-scoped session cookie.
            '/login': proxyToBoard(BOARD_80),
            '/logout': proxyToBoard(BOARD_80),
            // CSRF `HEAD /` is handled by the boardCsrf middleware, not
            // here, because `/` also serves the dev app.
          },
        }
      : {}),
    // `server.setup` runs before Rsbuild's own handlers, so the mock/CSRF
    // middleware registered here already precedes them.
    setup({ server }) {
      server.middlewares.use(API_BASE ? boardCsrf(BOARD_80) : mockApi());
    },
  },
});
