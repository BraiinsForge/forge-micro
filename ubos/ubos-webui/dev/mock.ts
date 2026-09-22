// Dev-only middleware: answers /api/* and the port-80 CSRF HEAD from
// in-memory fixtures so the UI runs with no board attached. Excluded
// from the production bundle (imported only by rsbuild.config.ts, never
// by the app). Set VITE_API_BASE to bypass this and proxy to a live
// board. It is registered through Rsbuild's `dev.setupMiddlewares`.

import type { IncomingMessage, ServerResponse } from 'node:http';
import type { ChainInfo, PauseReason } from '../web/lib/types';

type Middleware = (req: IncomingMessage, res: ServerResponse, next: () => void) => void;

let uptime = 1234;
let tick = 0;

// Dev session state for the mock. Starts armed so the dashboard loads
// without a login round-trip; POST /logout and the GET /api/__mock/expire
// toggle clear it, after which every /api/* answers 401; POST /login
// re-arms it.
let authed = true;

// The halt states `info()` cycles through, `null` (running) included.
const PAUSE_CYCLE: Array<PauseReason | null> = [null, 'dangerousTemperature', 'temperatureUnreadable'];

// A plausible /api/info body with light jitter so the chart animates.
function info() {
  tick += 1;
  uptime += 2;
  // The machine hashrate is the chains' own hashrates summed, rather than
  // an independent figure, so the dashboard's total and per-chain rows
  // never disagree.
  const hashrate = CHAINS.reduce((sum, chain) => sum + chain.hashrate, 0);
  const temperature = 62 + Math.round(Math.sin(tick / 9) * 3);
  const ambient = 28 + Math.round(Math.sin(tick / 13) * 2);
  const powerIn = 3200 + Math.round(Math.sin(tick / 7) * 120);
  // Cycles the halt so the thermal clear control and the
  // temperatureUnreadable label stay reachable against the mock, the
  // same way `ambient` cycles above without a live board.
  const pauseReason = PAUSE_CYCLE[Math.floor(tick / 20) % PAUSE_CYCLE.length];
  return {
    // The board name the API reports, not the brand: the brand reaches
    // the page from the declared brand directory, and a fixture that
    // named one would disagree with it.
    name: 'Mock Board',
    hashrate,
    uptime,
    temperature,
    ambient,
    chip_reading_stale: pauseReason !== null,
    fan_rpm: 4200,
    fan_pwm: 65,
    power_in_mw: powerIn * 1000,
    power_out_mw: (powerIn - 180) * 1000,
    psu_voltage_set_mv: 12000,
    psu_voltage_measured_mv: 11940,
    chain_voltage_mv: 0,
    psu_fault: false,
    pools: 0b001, // slot 0 alive
    net_confirm_seconds: 0,
    running: pauseReason === null,
    paused: pauseReason !== null,
    pause_reason: pauseReason,
    ip: '192.168.1.50',
    mac: 'de:ad:be:ef:00:01',
    time: new Date().toISOString().slice(0, 19).replace('T', ' '),
    ota: { runningSlot: 'A', runningVersion: 42, runningBuild: 'v0.3-12-gabc1234-260922120000', maxImageBytes: 952 * 1024 },
    chains: CHAINS,
  };
}

// One present, one absent, one faulted, and one unknown chain, so the
// dev loop shows every state the Hashboards card renders. Four entries
// is one more than MAX_CHAINS allows; a real board never reports the
// fourth. The mock runs the whole card, not the machine limit.
const CHAINS: ChainInfo[] = [
  { chain: 0, state: 'present', chips: 105, frequency_mhz: 500, hashrate: 6e11, hottest_c: 68 },
  { chain: 1, state: 'absent', chips: 0, frequency_mhz: 0, hashrate: 0, hottest_c: null },
  { chain: 2, state: 'faulted', chips: 0, frequency_mhz: 0, hashrate: 0, hottest_c: null },
  { chain: 3, state: 'unknown', chips: 0, frequency_mhz: 0, hashrate: 0, hottest_c: null },
];

const CONFIG = {
  pool: [
    {
      host: 'stratum.example.com',
      port: 3333,
      user: 'worker.001',
      protocol: { Sv1: { password: 'x' } },
      weight: 1,
    },
    {
      host: 'backup.example.com',
      port: 3333,
      user: 'worker.002',
      protocol: { Sv2: { pool_pubkey: null } },
      weight: 0,
    },
    null,
  ],
  thermal: {
    sample_interval_ms: 1000,
    dangerous_temp_c: 85,
    cooldown_temp_c: 75,
    dangerous_chip_temp_c: 105,
  },
  cooling: { mode: 'auto', target_temp_c: 65, fixed_fan_pwm: 60, min_fan_pwm: 20 },
  miner: { tuning: { Fixed: { frequency_mhz: 500, voltage_mv: 1200 } } },
  api: { username: 'admin', password: '' },
  network: { mode: 'Dhcp', hostname: '', address: [192, 168, 1, 50], prefix_len: 24, gateway: [192, 168, 1, 1], dns: [] },
};

const ASIC = { bounds: { frequencyMhz: { start: 200, end: 700 }, voltageMv: { start: 1000, end: 1400 } } };
const THERMAL = { bounds: { dangerousTempC: { start: 70, end: 110 }, cooldownTempC: { start: 50, end: 90 }, dangerousChipTempC: { start: 90, end: 125 } } };
const COOLING = { bounds: { targetTempC: { start: 40, end: 85 }, minFanPwm: { start: 0, end: 100 }, fixedFanPwm: { start: 0, end: 100 } } };
const NETWORK = { mode: 'Dhcp', address: [192, 168, 1, 50], prefix_len: 24, gateway: [192, 168, 1, 1], dns: [[8, 8, 8, 8]] };

const EVENTS_CATALOG = [
  { code: 1, fmt: 'Pool {} connected', args: ['u8'] },
  { code: 2, fmt: 'Temperature {}°C exceeds warn threshold', args: ['i16'] },
  { code: 3, fmt: 'Fan {} rpm below floor', args: ['u16'] },
];

// args are 22-char little-endian hex over an 11-byte payload.
const EVENTS = {
  events: [
    { seq: 1, code: 1, args: '0100000000000000000000', sev: 'info', ts: 1700000000 },
    { seq: 2, code: 2, args: '5a00000000000000000000', sev: 'warn', ts: 1700000600 },
    { seq: 3, code: 3, args: 'e803000000000000000000', sev: 'error', ts: 1700001200 },
  ],
};

function send(res: ServerResponse, body: unknown, headers?: Record<string, string>) {
  res.setHeader('Content-Type', 'application/json');
  if (headers) for (const [k, v] of Object.entries(headers)) res.setHeader(k, v);
  res.statusCode = 200;
  res.end(JSON.stringify(body));
}

export function mockApi(): Middleware {
  return (req, res, next) => {
    // req.url is undefined only for a malformed request line, which
    // never happens in dev; default to '/' so the split below is safe.
    const url = (req.url ?? '/').split('?')[0];

    // CSRF token endpoint.
    if (url === '/' && req.method === 'HEAD') {
      res.setHeader('X-CSRF-Token', 'dev-csrf-token');
      res.statusCode = 200;
      res.end();
      return;
    }

    // Session endpoints. The mock accepts any
    // credentials: POST /login arms the session and 302-redirects to
    // the dashboard, mirroring the board so the login form's native
    // submit lands on `/`; POST /logout clears it and returns to
    // /login.
    if (url === '/login' && req.method === 'POST') {
      authed = true;
      res.statusCode = 302;
      res.setHeader('Location', '/');
      res.end();
      return;
    }
    if (url === '/logout' && req.method === 'POST') {
      authed = false;
      res.statusCode = 302;
      res.setHeader('Location', '/login');
      res.end();
      return;
    }

    if (!url.startsWith('/api/')) return next();

    // Dev toggle: expire the session so the next /api call 401s,
    // exercising the SPA's redirect-to-login without a board. Open
    // http://localhost:5173/api/__mock/expire in another tab while
    // the dashboard is up; the Sign in button (POST /login) re-arms it.
    if (url === '/api/__mock/expire') {
      authed = false;
      send(res, { authed });
      return;
    }

    // Once expired, every /api/* answers 401 exactly like the board's
    // REST listener does for an anonymous request, driving the SPA's
    // session-expiry redirect.
    if (!authed) {
      res.statusCode = 401;
      res.setHeader('Content-Type', 'application/json');
      res.end(JSON.stringify({ message: 'unauthorized' }));
      return;
    }

    // Mutating endpoints just acknowledge.
    if (req.method === 'POST' || req.method === 'PATCH') {
      send(res, { ok: true });
      return;
    }

    switch (url) {
      case '/api/info': return send(res, info());
      case '/api/config': return send(res, CONFIG);
      case '/api/system/asic': return send(res, ASIC);
      case '/api/system/thermal': return send(res, THERMAL);
      case '/api/system/cooling': return send(res, COOLING);
      case '/api/system/network': return send(res, NETWORK);
      case '/api/system/events': return send(res, EVENTS);
      case '/api/system/events/catalog': return send(res, EVENTS_CATALOG);
      default:
        res.statusCode = 404;
        res.setHeader('Content-Type', 'application/json');
        res.end(JSON.stringify({ message: 'mock: no route for ' + url }));
    }
  };
}
