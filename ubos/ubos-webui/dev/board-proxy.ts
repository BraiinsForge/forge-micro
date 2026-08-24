// Dev-only helper for running against a real board (VITE_API_BASE set).
//
// The board splits its surface across two ports: JSON on 8080 and the
// page/CSRF/login on 80. `server.proxy` (in rsbuild.config.ts) forwards
// /api, /login and /logout, but the CSRF token is minted by a `HEAD /`
// on port 80 — and `/` also serves the dev app, so it can't be a blanket
// proxy entry. This middleware special-cases just `HEAD /`: it forwards
// the request (with the browser's session cookie) to the board and
// copies the `X-CSRF-Token` response header back, so mutating requests
// work in dev exactly as they do on the device. It is registered through
// Rsbuild's `dev.setupMiddlewares`.
import type { IncomingMessage, ServerResponse } from 'node:http';

type Middleware = (req: IncomingMessage, res: ServerResponse, next: () => void) => void;

export function boardCsrf(base80: string): Middleware {
  return async (req, res, next) => {
    if (req.method === 'HEAD' && (req.url === '/' || req.url === '/index.html')) {
      try {
        const upstream = await fetch(base80 + '/', {
          method: 'HEAD',
          redirect: 'manual',
          headers: req.headers.cookie ? { cookie: req.headers.cookie } : {},
        });
        const token = upstream.headers.get('x-csrf-token');
        if (token) res.setHeader('X-CSRF-Token', token);
        res.statusCode = 200;
        res.end();
      } catch {
        res.statusCode = 502;
        res.end('board unreachable');
      }
      return;
    }
    next();
  };
}
