// REST client for the board API.
//
// All data lives on the REST listener on port 8080 (cross-origin from
// the port-80 page); the CSRF token is minted by a HEAD / against the
// page origin (port 80). In dev, both are same-origin relative paths so
// the Vite mock/proxy can intercept them.
//
// The REST worker pool is shallow (often one connection deep), so every
// mutating call retries with a short backoff, matching the vanilla UI.

import type { Config, ConfigPatch, Info } from './types';

const API = import.meta.env.DEV
  ? ''
  : `${location.protocol}//${location.hostname}:8080`;

export const RETRY_DELAYS = [0, 250, 500, 1000];
const sleep = (ms: number): Promise<void> => new Promise((resolve) => setTimeout(resolve, ms));

// A 401 from the API means the session is gone — expired, invalidated,
// or cleared by a reboot. Bounce to the login page with a marker so it
// explains why, instead of leaving the SPA showing dead requests. The
// flag collapses several concurrent 401s into a single navigation, and
// `replace` keeps the dead SPA out of the history stack.
let redirecting = false;
export function loginRedirect(): void {
  if (redirecting) return;
  redirecting = true;
  location.replace('/login?expired=1');
}

// GET JSON from an API path. Throws on a non-ok response.
export async function getJSON<T = unknown>(path: string): Promise<T> {
  const response = await fetch(API + path, {
    cache: 'no-store',
    credentials: 'include',
  });
  if (response.status === 401) {
    loginRedirect();
    throw new Error('unauthorized');
  }
  if (!response.ok) throw new Error('HTTP ' + response.status);
  return response.json() as Promise<T>;
}

export const getInfo = (): Promise<Info> => getJSON<Info>('/api/info');
export const getConfig = (): Promise<Config> => getJSON<Config>('/api/config');

// Fetch the single-use CSRF token from the page origin (port 80).
export async function csrfToken(): Promise<string> {
  const response = await fetch('/', { method: 'HEAD', cache: 'no-store' });
  return response.headers.get('X-CSRF-Token') || '';
}

// POST JSON to an API path with a CSRF token, retrying against the
// shallow pool. Surfaces the API's {message} on a 4xx; stops retrying
// on any 4xx since the board rejects it identically on retry.
export async function postJSON(path: string, body: unknown): Promise<Response> {
  let lastError: unknown;
  for (const wait of RETRY_DELAYS) {
    if (wait) await sleep(wait);
    try {
      const csrf = await csrfToken();
      const response = await fetch(API + path, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': csrf },
        body: JSON.stringify(body),
        credentials: 'include',
      });
      if (response.ok) return response;
      // A 401 means the session died; redirect and stop retrying (the
      // 4xx break below handles the loop exit).
      if (response.status === 401) loginRedirect();
      let detail = '';
      try {
        const data = await response.json();
        if (data && data.message) detail = data.message;
      } catch (_) {
        /* body was not JSON */
      }
      lastError = new Error(detail || 'HTTP ' + response.status);
      if (response.status >= 400 && response.status < 500) break;
    } catch (error) {
      lastError = error;
    }
  }
  throw lastError || new Error('unknown');
}

// Apply a partial config object via POST /api/config.
export const postConfig = (body: ConfigPatch): Promise<Response> => postJSON('/api/config', body);

// Options for a single-shot POST. `json` sends a JSON body (and sets the
// content type); `body`/`headers` carry a raw payload for the OTA chunk
// uploader; `signal` lets the caller abort.
export interface PostOnceOptions {
  body?: BodyInit | null;
  headers?: Record<string, string>;
  signal?: AbortSignal;
  json?: unknown;
}

// Single-shot POST with a CSRF token, no retry. For commands whose ack
// arrives before the connection drops (network switch, restart, factory
// reset): a retry after the drop would re-fire the command, so the
// caller inspects the returned Response (or catches the throw) instead.
// Optional `headers`/`body`/`signal` cover the OTA chunk uploader.
export async function postOnce(path: string, { body, headers, signal, json }: PostOnceOptions = {}): Promise<Response> {
  const csrf = await csrfToken();
  const base = { 'X-CSRF-Token': csrf };
  const response = await fetch(API + path, {
    method: 'POST',
    credentials: 'include',
    headers: json ? { 'Content-Type': 'application/json', ...base } : { ...base, ...headers },
    body: json ? JSON.stringify(json) : body,
    signal,
  });
  if (response.status === 401) loginRedirect();
  return response;
}

// The API base, for callers that need to build a request directly
// (e.g. the OTA continuation chunks, which are CSRF-exempt).
export { API };
