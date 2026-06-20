// Dashboard SPA. Polls /api/info every 2 s for the live tiles and the
// rolling charts, and reads /api/config on demand for the Configuration
// page. All `/api/*` calls go to port 8080 (ubos-api); the static
// shell, login, and logout live on the same port 80 we were served from.
const API = `${location.protocol}//${location.hostname}:8080`;

// Chart geometry. PAD_* are fixed pixel gutters reserved for axis
// labels; the chart width and height are read from each SVG element's
// rendered box at draw time so the coordinate space always matches the
// actual pixel box and the viewBox never letterboxes.
const SAMPLE_COUNT = 150;
const PAD_LEFT = 52;
const PAD_RIGHT = 12;
const PAD_TOP = 10;
const PAD_BOTTOM = 20;

// Return the coordinate-space dimensions for a chart SVG element.
// Reads the element's rendered size so the viewBox matches exactly and
// no letterboxing occurs. Returns null when the element is not yet
// laid out (zero width or height), which signals the caller to skip
// drawing until the next resize event delivers real dimensions.
function chartDims(svg) {
  const width = Math.round(svg.clientWidth);
  const height = Math.round(svg.clientHeight);
  if (width === 0 || height === 0) return null;
  return {
    width,
    height,
    plotLeft: PAD_LEFT,
    plotTop: PAD_TOP,
    plotWidth: width - PAD_LEFT - PAD_RIGHT,
    plotHeight: height - PAD_TOP - PAD_BOTTOM,
  };
}

// Rolling sample buffers: oldest at index 0, newest at the end.
const hrBuf = new Array(SAMPLE_COUNT).fill(null);
const tpBuf = new Array(SAMPLE_COUNT).fill(null);
const inBuf = new Array(SAMPLE_COUNT).fill(null);
const outBuf = new Array(SAMPLE_COUNT).fill(null);

// Per-slot pool meta cache. undefined = not yet fetched (fall back to
// Dead on a clear bit), null = fetched and slot is empty, object =
// fetched and configured. Populated by loadPoolMeta() and refreshed by
// loadCfg() and the save handler.
const poolCfg = [undefined, undefined, undefined];

// Snapshot of the Temperature form values as loaded from GET /api/config.
// The submit handler sends `thermal` and `cooling_target_temp_c` only when
// they actually changed, so editing one threshold does not drag the others
// through the cross-field validator.
let tempInit = null;

const $ = id => document.getElementById(id);
const $$ = (parent, selector) => parent.querySelector(selector);

let cfgLoaded = false;
let cfgLoading = false;
let networkStatusLoading = false;
let asicLoaded = false;
let thermalLoaded = false;
let coolingLoaded = false;

// Constrain the frequency/voltage inputs from GET /api/system/asic so
// the accepted range has a single source of truth in ubos-config::mining.
async function loadAsicBounds() {
  if (asicLoaded) return true;
  const response = await fetch(API + '/api/system/asic', { cache: 'no-store', credentials: 'include' });
  if (!response.ok) throw new Error('HTTP ' + response.status);
  const data = await response.json();
  const bounds = data.bounds || {};
  const freqInput = $('freqMhz');
  const voltInput = $('voltMv');
  if (bounds.frequencyMhz) {
    freqInput.min = bounds.frequencyMhz.start;
    freqInput.max = bounds.frequencyMhz.end;
  }
  if (bounds.voltageMv) {
    voltInput.min = bounds.voltageMv.start;
    voltInput.max = bounds.voltageMv.end;
  }
  asicLoaded = true;
  return true;
}

// Same pattern as loadAsicBounds: pull the accepted ranges for the
// dangerC and cooldownC inputs from GET /api/system/thermal so the
// bounds live only in ubos-config::thermal. The sample interval is
// not user-editable, so its bounds are ignored here.
async function loadThermalBounds() {
  if (thermalLoaded) return true;
  const response = await fetch(API + '/api/system/thermal', { cache: 'no-store', credentials: 'include' });
  if (!response.ok) throw new Error('HTTP ' + response.status);
  const data = await response.json();
  const bounds = data.bounds || {};
  const dangerInput = $('dangerC');
  const cooldownInput = $('cooldownC');
  if (bounds.dangerousTempC) {
    dangerInput.min = bounds.dangerousTempC.start;
    dangerInput.max = bounds.dangerousTempC.end;
  }
  if (bounds.cooldownTempC) {
    cooldownInput.min = bounds.cooldownTempC.start;
    cooldownInput.max = bounds.cooldownTempC.end;
  }
  thermalLoaded = true;
  return true;
}

// Pull the accepted range for the targetC input from GET /api/system/cooling
// so the bound lives only in ubos-config::cooling.
async function loadCoolingBounds() {
  if (coolingLoaded) return true;
  const response = await fetch(API + '/api/system/cooling', { cache: 'no-store', credentials: 'include' });
  if (!response.ok) throw new Error('HTTP ' + response.status);
  const data = await response.json();
  const bounds = data.bounds || {};
  const targetInput = $('targetC');
  if (bounds.targetTempC) {
    targetInput.min = bounds.targetTempC.start;
    targetInput.max = bounds.targetTempC.end;
  }
  coolingLoaded = true;
  return true;
}

const CFG_SECTIONS = {
  pools: 'secPools',
  temperature: 'secTemp',
  mining: 'secMining',
  system: 'secSystem',
  network: 'secNetwork',
};

const CFG_NAV = {
  pools: 'snPools',
  temperature: 'snTemp',
  mining: 'snMining',
  system: 'snSystem',
  network: 'snNetwork',
};

function route() {
  const hash = location.hash;
  const isCfg = hash === '#config' || hash.startsWith('#config/');
  $('dash').classList.toggle('hide', isCfg);
  $('cfg').classList.toggle('hide', !isCfg);
  $('nDash').classList.toggle('on', !isCfg);
  $('nCfg').classList.toggle('on', isCfg);
  if (!isCfg) return;
  const sub = (hash.startsWith('#config/') ? hash.slice(8) : '') || 'pools';
  const secKey = CFG_SECTIONS[sub] ? sub : 'pools';
  Object.keys(CFG_SECTIONS).forEach(key => {
    $(CFG_SECTIONS[key]).classList.toggle('on', key === secKey);
    $(CFG_NAV[key]).classList.toggle('on', key === secKey);
  });
  if (!cfgLoaded && !cfgLoading) loadCfg();
  if (secKey === 'network') loadNetworkStatus();
}
addEventListener('hashchange', route);
route();

function fmtHr(hashes) {
  if (hashes >= 1e12) return (hashes / 1e12).toFixed(2) + ' TH/s';
  if (hashes >= 1e9) return (hashes / 1e9).toFixed(2) + ' GH/s';
  if (hashes >= 1e6) return (hashes / 1e6).toFixed(2) + ' MH/s';
  if (hashes >= 1e3) return (hashes / 1e3).toFixed(2) + ' kH/s';
  return hashes.toFixed(0) + ' H/s';
}

function fmtUp(seconds) {
  const days = (seconds / 86400) | 0;
  const hours = (seconds / 3600) % 24 | 0;
  const minutes = (seconds / 60) % 60 | 0;
  if (days) return `${days}d ${hours}h ${minutes}m`;
  if (hours) return `${hours}h ${minutes}m`;
  return `${minutes}m ${seconds % 60 | 0}s`;
}

// Round a positive number up to the next 1, 2, or 5 times a power of
// ten so gridline intervals stay readable at any scale.
function niceStep(raw) {
  const pow = Math.pow(10, Math.floor(Math.log10(raw)));
  const norm = raw / pow;
  const mult = norm <= 1 ? 1 : norm <= 2 ? 2 : norm <= 5 ? 5 : 10;
  return mult * pow;
}

// Auto-range the Y axis top. The configured max is only a starting
// point: find the peak sample across the window and, when it leaves the
// 30% to 95% band of the current range, rescale so the peak sits near
// mid-axis. min stays fixed at the configured baseline. The band keeps
// the axis steady at steady state, and centering lands the trace inside
// the 1/3 to 2/3 sweet spot so it does not oscillate.
function autoRange(chart) {
  let peak = null;
  for (const line of chart.lines) {
    for (let index = 0; index < SAMPLE_COUNT; index++) {
      const value = line.buf[index];
      if (value == null) continue;
      if (peak == null || value > peak) peak = value;
    }
  }
  if (peak == null) return;
  const frac = (peak - chart.min) / (chart.max - chart.min);
  if (frac >= 0.30 && frac <= 0.95) return;
  const desiredSpan = Math.max((peak - chart.min) * 2, chart.minSpan);
  const majStep = niceStep(desiredSpan / 4);
  const divs = Math.max(2, Math.ceil(desiredSpan / majStep));
  chart.max = chart.min + majStep * divs;
  chart.maj = majStep;
  chart.minTick = majStep / 2;
}

const charts = [
  {
    id: 'cHr',
    min: 0,
    max: 2e12,
    maj: 1e12,
    minTick: 5e11,
    minSpan: 2e11,
    axis: value => { const t = value / 1e12; return (Number.isInteger(t) ? t.toFixed(0) : t.toFixed(1)) + 'T'; },
    val: fmtHr,
    lines: [{ cls: 'hrL', buf: hrBuf, lbl: 'Hashrate' }],
  },
  {
    id: 'cTp',
    min: 20,
    max: 100,
    maj: 20,
    minTick: 10,
    minSpan: 20,
    axis: value => value + '°C',
    val: value => value + '°C',
    lines: [{ cls: 'tpL', buf: tpBuf, lbl: 'Temperature' }],
  },
  {
    id: 'cPw',
    min: 0,
    max: 60,
    maj: 20,
    minTick: 10,
    minSpan: 10,
    axis: value => value + 'W',
    val: value => value.toFixed(0) + ' W',
    lines: [
      { cls: 'inL', buf: inBuf, lbl: 'Power IN' },
      { cls: 'outL', buf: outBuf, lbl: 'Power OUT' },
    ],
  },
];

function drawChart(chart) {
  const root = $(chart.id);
  const svg = $$(root, 'svg');
  const dims = chartDims(svg);
  if (!dims) return;
  svg.setAttribute('viewBox', `0 0 ${dims.width} ${dims.height}`);
  autoRange(chart);
  const { width, height, plotLeft, plotTop, plotWidth, plotHeight } = dims;
  const span = chart.max - chart.min;
  const steps = Math.round(span / chart.minTick);
  const majEvery = Math.round(chart.maj / chart.minTick);
  let gridSvg = '';
  let yAxisSvg = '';
  let xAxisSvg = '';
  for (let step = 0; step <= steps; step++) {
    const pixelY = plotTop + plotHeight - (step / steps) * plotHeight;
    const isMajor = step % majEvery === 0;
    if (isMajor) {
      const value = chart.min + step * chart.minTick;
      gridSvg += `<line class="gr" x1="${plotLeft}" y1="${pixelY}" x2="${plotLeft + plotWidth}" y2="${pixelY}"/>`;
      yAxisSvg += `<text class="ax" x="${plotLeft - 6}" y="${pixelY + 3}" text-anchor="end">${chart.axis(value)}</text>`;
      continue;
    }
    yAxisSvg += `<line class="tk" x1="${plotLeft - 3}" y1="${pixelY}" x2="${plotLeft}" y2="${pixelY}"/>`;
  }
  for (let minute = 0; minute <= 5; minute++) {
    const pixelX = plotLeft + (minute / 5) * plotWidth;
    gridSvg += `<line class="gr" x1="${pixelX}" y1="${plotTop}" x2="${pixelX}" y2="${plotTop + plotHeight}"/>`;
    xAxisSvg += `<text class="ax" x="${pixelX}" y="${height - 6}" text-anchor="middle">-${5 - minute}m</text>`;
  }
  $$(root, '.g').innerHTML = gridSvg;
  $$(root, '.y').innerHTML = yAxisSvg;
  $$(root, '.x').innerHTML = xAxisSvg;
  $$(root, '.lines').innerHTML = chart.lines.map(line => {
    let path = '';
    let penDown = false;
    for (let index = 0; index < SAMPLE_COUNT; index++) {
      const value = line.buf[index];
      if (value == null) {
        penDown = false;
        continue;
      }
      const clamped = Math.min(Math.max(value, chart.min), chart.max);
      const pixelX = (plotLeft + index * plotWidth / (SAMPLE_COUNT - 1)).toFixed(1);
      const pixelY = (plotTop + plotHeight - ((clamped - chart.min) / span) * plotHeight).toFixed(1);
      path += (penDown ? 'L' : 'M') + pixelX + ',' + pixelY + ' ';
      penDown = true;
    }
    return `<path class="${line.cls}" d="${path}"/>`;
  }).join('');
}

function draw() {
  charts.forEach(drawChart);
}

function setIcon(node, alive) {
  const use = node.querySelector('.dot use');
  if (use) use.setAttribute('href', alive ? '#sok' : '#serr');
}

function setLk(alive) {
  const badge = $('lk');
  const card = badge.closest('.lkc');
  badge.textContent = alive ? 'Alive' : 'Dead';
  card.classList.toggle('live', alive);
  card.classList.toggle('dead', !alive);
  setIcon(card, alive);
}

// The Link badge tolerates a few dropped polls before showing Dead:
// the one-connection REST listener occasionally loses a race against
// another fetch, and a single miss is not a real outage.
const LINK_FAIL_THRESHOLD = 3;
let linkFailures = 0;

// Open the network confirm modal. Called with no args for the review
// step before a switch, or with (ip, seconds) to show the pending
// confirm-or-revert state with a live countdown driven by the poll.
function showModal(ip, seconds) {
  const pending = seconds != null;
  if (pending) {
    const minutes = Math.floor(seconds / 60);
    const remaining = String(seconds % 60).padStart(2, '0');
    $('netModalPendingText').textContent =
      'Applied ' + ip + '. Confirm within ' + minutes + ':' + remaining + ' or it reverts.';
  }
  $('netModalReview').classList.toggle('hide', pending);
  $('netModalPending').classList.toggle('hide', !pending);
  $('netModal').classList.remove('hide');
}

let pendingNetSwitch = null;
// True while an apply POST is in flight, so reopening the form mid-apply
// cannot fire a second concurrent POST /api/system/network.
let applyInFlight = false;
// The hostname currently persisted, captured on config load. A hostname
// change is applied to the stack live but the mDNS responder only reads it
// at boot, so the apply notice tells the operator a reboot is needed for the
// new .local name to take effect.
let appliedHostname = '';

async function tick() {
  // Skip the poll while the Configuration page is loading. The REST
  // listener pool is shallow (one connection deep on the tight boards),
  // so this 2 s poll and the config page's fetches knock each other over;
  // pausing here is the main fix for the transient Configuration "Load
  // failed" races.
  if (cfgLoading) return;
  try {
    const response = await fetch(API + '/api/info', { cache: 'no-store', credentials: 'include' });
    if (!response.ok) throw 0;
    const data = await response.json();
    linkFailures = 0;
    setLk(true);
    if (data.name && document.title !== data.name) document.title = data.name;
    $('hr').textContent = fmtHr(data.hashrate);
    $('ip').textContent = data.ip || '–';
    $('t').textContent = data.time || '–';
    $('up').textContent = fmtUp(data.uptime);
    $('tp').textContent = data.temperature ? data.temperature + ' °C' : '–';
    $('fs').textContent = data.fan_rpm ? data.fan_rpm + ' RPM' : '–';
    $('fp').textContent = data.fan_pwm ? data.fan_pwm + ' %' : '–';
    // Zero reads render as '–' to match the fan tiles: the cold-boot
    // window before the first PSU sample is indistinguishable from a
    // genuine 0 mW / 0 mV reading, and treating both as "no data"
    // avoids a misleading 0.0 W flash during boot. The power chart
    // below still pushes the literal value so the trace stays
    // continuous; the two displays serve different needs.
    $('pwrIn').textContent = data.power_in_mw ? (data.power_in_mw / 1000).toFixed(1) + ' W' : '–';
    $('pwrOut').textContent = data.power_out_mw ? (data.power_out_mw / 1000).toFixed(1) + ' W' : '–';
    $('vSet').textContent = data.voltage_set_mv ? (data.voltage_set_mv / 1000).toFixed(3) + ' V' : '–';
    $('vMeas').textContent = data.voltage_measured_mv ? (data.voltage_measured_mv / 1000).toFixed(3) + ' V' : '–';
    const poolMask = data.pools | 0;
    for (let slot = 0; slot < 3; slot++) {
      const element = $('po' + (slot + 1));
      const alive = (poolMask >> slot) & 1;
      const cached = poolCfg[slot];
      element.classList.toggle('live', !!alive);
      element.classList.toggle('empty', !alive && cached === null);
      element.querySelector('.pt').textContent = alive ? 'Alive' : (cached === null ? 'Empty' : 'Dead');
      setIcon(element, !!alive);
    }
    hrBuf.shift(); hrBuf.push(data.hashrate ?? null);
    tpBuf.shift(); tpBuf.push(data.temperature ?? null);
    inBuf.shift(); inBuf.push(data.power_in_mw == null ? null : data.power_in_mw / 1000);
    outBuf.shift(); outBuf.push(data.power_out_mw == null ? null : data.power_out_mw / 1000);
    draw();
    // Drive the global confirm modal from the poll so it reappears on any
    // page after a reload while a network change awaits confirmation.
    const modal = $('netModal');
    if (modal) {
      const confirmedSeconds = (data.net_confirm_seconds | 0);
      if (confirmedSeconds > 0 && !pendingNetSwitch) {
        showModal(data.ip || '', confirmedSeconds);
      } else if (!$('netModalPending').classList.contains('hide')) {
        $('netModal').classList.add('hide');
      }
    }
  } catch (error) {
    // Only flip to Dead after several consecutive misses so a single
    // dropped poll does not flash Dead on an otherwise healthy board.
    linkFailures++;
    if (linkFailures >= LINK_FAIL_THRESHOLD) setLk(false);
    console.warn('tick fetch failed:', error);
  }
}
tick().then(loadPoolMeta);
let pollTimer = setInterval(tick, 2000);

function renderPoolMeta() {
  for (let slot = 0; slot < 3; slot++) {
    const cached = poolCfg[slot];
    const info = $('po' + (slot + 1)).querySelector('.pi');
    if (cached === undefined) {
      info.textContent = '';
      continue;
    }
    if (cached === null) {
      info.textContent = 'not configured';
      continue;
    }
    info.textContent = `${cached.host}:${cached.port} · ${cached.sv2 ? 'SV2' : 'SV1'}${cached.user ? ` · ${cached.user}` : ''}`;
  }
}

async function loadPoolMeta() {
  // Same retry budget as loadCfg: the REST listener's worker pool is
  // finite (one worker on the smallest boards), so a /api/info tick in
  // flight can still knock this fetch over. A short backoff keeps the
  // dashboard labels populated without spinning when the board is
  // genuinely offline.
  const delays = [0, 250, 500, 1000];
  for (const wait of delays) {
    if (wait) await new Promise(resolve => setTimeout(resolve, wait));
    try {
      const response = await fetch(API + '/api/config', { cache: 'no-store', credentials: 'include' });
      if (!response.ok) continue;
      const data = await response.json();
      const pools = data.pool || [];
      for (let slot = 0; slot < 3; slot++) {
        const pool = pools[slot];
        if (!pool) {
          poolCfg[slot] = null;
          continue;
        }
        poolCfg[slot] = {
          host: pool.host,
          port: pool.port,
          user: pool.user || '',
          sv2: !!(pool.protocol && pool.protocol.Sv2),
        };
      }
      renderPoolMeta();
      return;
    } catch (_) {
      // retry on next backoff
    }
  }
}

const POOL_RE = /^(?:(stratum2?\+tcp):\/\/)?([^:\/]+)(?::(\d+))?$/;

function bytesToHex(bytes) {
  return Array.from(bytes, value => value.toString(16).padStart(2, '0')).join('');
}

function hexToBytes(hex) {
  const bytes = [];
  for (let index = 0; index < hex.length; index += 2) {
    bytes.push(parseInt(hex.slice(index, index + 2), 16));
  }
  return bytes;
}

// Parse a dotted-quad IPv4 string into a 4-element byte array.
// Returns null if the string is not a valid IPv4 address.
function ipToBytes(str) {
  const parts = str.trim().split('.');
  if (parts.length !== 4) return null;
  const bytes = parts.map(Number);
  if (bytes.some(b => !Number.isInteger(b) || b < 0 || b > 255)) return null;
  return bytes;
}

// Format a 4-element byte array as a dotted-quad IPv4 string.
function bytesToIp(bytes) {
  return bytes.join('.');
}

// Convert a prefix length (1–32) to a dotted-quad netmask string.
function prefixToMask(prefix) {
  const bits = prefix >>> 0;
  const value = bits === 0 ? 0 : (0xffffffff << (32 - bits)) >>> 0;
  return [(value >>> 24) & 0xff, (value >>> 16) & 0xff, (value >>> 8) & 0xff, value & 0xff].join('.');
}

// Convert a dotted-quad netmask string to a prefix length (1–32).
// Returns null when the string is not a valid contiguous netmask.
function maskToPrefix(maskStr) {
  const bytes = ipToBytes(maskStr);
  if (!bytes) return null;
  const value = ((bytes[0] << 24) | (bytes[1] << 16) | (bytes[2] << 8) | bytes[3]) >>> 0;
  // A valid mask is a run of 1-bits followed by 0-bits with no gaps.
  const inverted = (~value) >>> 0;
  if ((inverted & (inverted + 1)) !== 0) return null;
  // Count leading 1-bits to derive the prefix length.
  const leading = Math.clz32(~value >>> 0);
  if (leading < 1 || leading > 32) return null;
  return leading;
}

// Reflect the current mode in the segmented button pair and set the
// disabled state on the address/mask/gateway/DNS inputs. The address
// block stays visible in both modes; fields are read-only in DHCP mode.
function syncNetUi() {
  const mode = $('netMode').value;
  const isStatic = mode === 'Static';
  $('netModeDhcp').classList.toggle('active', !isStatic);
  $('netModeStatic').classList.toggle('active', isStatic);
  for (const id of ['netAddr', 'netMask', 'netGw', 'netDns1', 'netDns2']) {
    $(id).disabled = !isStatic;
  }
}

$('netModeDhcp').addEventListener('click', function () {
  $('netMode').value = this.dataset.mode;
  syncNetUi();
});
$('netModeStatic').addEventListener('click', function () {
  $('netMode').value = this.dataset.mode;
  syncNetUi();
});
syncNetUi();

// Reflect the selected tuner algorithm in the segmented button pair.
// The hidden tunerAlgo input holds the serde variant name the POST
// /api/config body carries under miner.algorithm.
function syncTunerUi() {
  const algorithm = $('tunerAlgo').value;
  $('tunerFixed').classList.toggle('active', algorithm === 'Fixed');
}

$('tunerFixed').addEventListener('click', function () {
  $('tunerAlgo').value = this.dataset.algo;
  syncTunerUi();
});
syncTunerUi();

function syncPoolUi(slot) {
  const root = $('pcfg' + slot);
  const form = $('cfgForm');
  const value = (form['url' + slot].value || '').trim();
  const match = value.match(POOL_RE);
  // Empty URL stays empty so the CSS keeps both protocol-specific
  // blocks hidden; the protocol is unknown until the user types
  // something the regex recognizes.
  root.dataset.protocol = value
    ? (match && match[1] === 'stratum2+tcp' ? 'sv2' : 'sv1')
    : '';
  root.dataset.trust = form['trust' + slot].checked ? 'yes' : 'no';
}

async function loadCfg() {
  // The REST listener serves connections one-at-a-time, so a click on
  // Configuration that lands while the 2 s /api/info poll is in flight
  // can fail at the TCP layer. Retry the GET a few times with backoff
  // before reporting Load failed, so the operator does not have to
  // bounce out of the tab to recover from a transient race.
  // Each section has its own .msg status div. Mirror load status to
  // all five so the operator sees the result on whichever section they
  // happen to be viewing when the load finishes.
  const msgIds = ['msg', 'msgTemp', 'msgMining', 'msgSystem', 'msgNetwork'];
  const setMsg = (color, text) => msgIds.forEach(id => {
    const element = $(id);
    element.style.color = color;
    element.textContent = text;
  });
  cfgLoading = true;
  const delays = [0, 250, 500, 1000];
  let lastError;
  try {
    for (const wait of delays) {
      if (wait) await new Promise(resolve => setTimeout(resolve, wait));
      try {
        await loadAsicBounds();
        await loadThermalBounds();
        await loadCoolingBounds();
        const response = await fetch(API + '/api/config', { cache: 'no-store', credentials: 'include' });
        if (!response.ok) throw new Error('HTTP ' + response.status);
        const data = await response.json();
        const form = $('cfgForm');
        const pools = data.pool || [];
        for (let index = 0; index < 3; index++) {
          const pool = pools[index];
          const slot = index + 1;
          const sv1 = pool && pool.protocol && pool.protocol.Sv1;
          const sv2 = pool && pool.protocol && pool.protocol.Sv2;
          const scheme = sv2 ? 'stratum2+tcp' : 'stratum+tcp';
          form['url' + slot].value = pool ? `${scheme}://${pool.host}:${pool.port}` : '';
          form['user' + slot].value = pool && pool.user ? pool.user : '';
          form['pass' + slot].value = sv1 && sv1.password ? sv1.password : '';
          form['trust' + slot].checked = !!(sv2 && !sv2.pool_pubkey);
          form['key' + slot].value = sv2 && sv2.pool_pubkey ? bytesToHex(sv2.pool_pubkey) : '';
          syncPoolUi(slot);
          poolCfg[index] = pool ? {
            host: pool.host,
            port: pool.port,
            user: pool.user || '',
            sv2: !!sv2,
          } : null;
        }
        renderPoolMeta();
        if (data.thermal) {
          $('dangerC').value = data.thermal.dangerous_temp_c;
          $('cooldownC').value = data.thermal.cooldown_temp_c;
        }
        if (data.cooling_target_temp_c != null) {
          $('targetC').value = data.cooling_target_temp_c;
        }
        tempInit = {
          sampleMs: data.thermal ? data.thermal.sample_interval_ms : null,
          dangerC: data.thermal ? data.thermal.dangerous_temp_c : null,
          cooldownC: data.thermal ? data.thermal.cooldown_temp_c : null,
          targetC: data.cooling_target_temp_c != null ? data.cooling_target_temp_c : null,
        };
        if (data.tuning) {
          $('freqMhz').value = data.tuning.frequency_mhz;
          $('voltMv').value = data.tuning.voltage_mv;
        }
        if (data.miner) {
          $('tunerAlgo').value = data.miner.algorithm;
          syncTunerUi();
        }
        if (data.api) {
          $('sysUser').value = data.api.username || '';
        }
        if (data.network) {
          appliedHostname = data.network.hostname || '';
          $('netHostname').value = appliedHostname;
        }
        cfgLoaded = true;
        setMsg('', '');
        return;
      } catch (error) {
        lastError = error;
        console.warn('loadCfg attempt failed:', error);
      }
    }
    setMsg('var(--red)', 'Load failed: ' + ((lastError && lastError.message) || 'unknown'));
  } finally {
    cfgLoading = false;
  }
}

for (let slot = 1; slot <= 3; slot++) {
  const form = $('cfgForm');
  form['url' + slot].addEventListener('input', () => syncPoolUi(slot));
  form['trust' + slot].addEventListener('change', () => syncPoolUi(slot));
}

charts.forEach(chart => {
  const root = $(chart.id);
  const svg = $$(root, 'svg');
  const tip = $$(root, '.tip');
  const valueText = $$(root, '.va');
  svg.addEventListener('pointermove', event => {
    if (event.pointerType !== 'mouse') return;
    const dims = chartDims(svg);
    if (!dims) return;
    const { plotLeft, plotTop, plotWidth, plotHeight } = dims;
    // Map the pointer's CSS-pixel position into viewBox coordinates.
    // clientWidth equals the viewBox width (1 unit = 1 CSS pixel), so
    // no scale factor is needed beyond the element's own bounding rect.
    const rect = svg.getBoundingClientRect();
    const viewX = (event.clientX - rect.left) * (dims.width / rect.width);
    let index = Math.round((viewX - plotLeft) * (SAMPLE_COUNT - 1) / plotWidth);
    if (index < 0) index = 0;
    if (index >= SAMPLE_COUNT) index = SAMPLE_COUNT - 1;
    const pixelX = plotLeft + index * plotWidth / (SAMPLE_COUNT - 1);
    const secondsAgo = (SAMPLE_COUNT - 1 - index) * 2;
    const span = chart.max - chart.min;
    const circles = chart.lines.map(line => {
      const value = line.buf[index];
      if (value == null) return '';
      const clamped = Math.min(Math.max(value, chart.min), chart.max);
      const pixelY = plotTop + plotHeight - ((clamped - chart.min) / span) * plotHeight;
      return `<circle class="${line.cls}" cx="${pixelX}" cy="${pixelY.toFixed(1)}" r="3"/>`;
    }).join('');
    tip.innerHTML = `<line class="cur" x1="${pixelX}" y1="${plotTop}" x2="${pixelX}" y2="${plotTop + plotHeight}"/>${circles}`;
    const timestamp = 'T-' + (secondsAgo < 60 ? secondsAgo + 's' : (secondsAgo / 60 | 0) + 'm ' + (secondsAgo % 60) + 's');
    const parts = chart.lines
      .map(line => {
        if (line.buf[index] == null) return null;
        return chart.lines.length > 1 ? `${line.lbl}: ${chart.val(line.buf[index])}` : chart.val(line.buf[index]);
      })
      .filter(Boolean);
    valueText.textContent = parts.length ? `${timestamp}, ${parts.join(', ')}` : timestamp;
  });
  svg.addEventListener('pointerleave', () => {
    tip.innerHTML = '';
    valueText.textContent = '';
  });
});

// Redraw all charts whenever any chart SVG changes size so the
// coordinate space always matches the rendered box. A single observer
// covers all three SVGs; the callback redraws every chart because
// mobile rotations and desktop window resizes affect all of them.
const chartResizeObserver = new ResizeObserver(() => draw());
charts.forEach(chart => {
  const svg = $$($(chart.id), 'svg');
  chartResizeObserver.observe(svg);
});

// Fetch a CSRF token by issuing a HEAD request to the document root.
async function csrfToken() {
  const response = await fetch('/', { method: 'HEAD', cache: 'no-store' });
  return response.headers.get('X-CSRF-Token') || '';
}

// POST JSON to an arbitrary API path with the CSRF token. Pass a
// pre-fetched token to keep the token request separate from the POST,
// so a caller can tell a token failure apart from a dropped POST.
async function csrfPostTo(path, body, token) {
  const csrf = token === undefined ? await csrfToken() : token;
  return fetch(API + path, {
    method: 'POST',
    headers: {
      'Content-Type': 'application/json',
      'X-CSRF-Token': csrf,
    },
    body: JSON.stringify(body),
    credentials: 'include',
  });
}

async function csrfPost(body) {
  return csrfPostTo('/api/config', body);
}

// POST with backoff against the shallow REST listener pool, which refuses
// a request that loses the race with the 2 s status poll when every
// worker is busy. Takes a thunk so the caller owns the path and body;
// returns the ok response or throws the last error. Stops early on a 4xx,
// which the board rejects identically on retry, so retrying would only
// burn the backoff.
async function postWithRetry(send) {
  const delays = [0, 250, 500, 1000];
  let lastError;
  for (const wait of delays) {
    if (wait) await new Promise(resolve => setTimeout(resolve, wait));
    try {
      const response = await send();
      if (response.ok) return response;
      lastError = new Error('HTTP ' + response.status);
      if (response.status >= 400 && response.status < 500) break;
    } catch (error) {
      lastError = error;
    }
  }
  throw lastError || new Error('unknown');
}

$('cfgForm').addEventListener('submit', async event => {
  event.preventDefault();
  const msg = $('msg');
  msg.style.color = '';
  const formData = new FormData(event.target);
  const pool = [];
  for (let slot = 1; slot <= 3; slot++) {
    const url = (formData.get('url' + slot) || '').trim();
    if (!url) continue;
    const match = url.match(POOL_RE);
    if (!match) {
      msg.style.color = 'var(--red)';
      msg.textContent = 'Invalid pool ' + slot + ' URL';
      return;
    }
    const host = match[2];
    const port = match[3] ? +match[3] : 3333;
    const user = formData.get('user' + slot) || '';
    let protocol;
    if (match[1] === 'stratum2+tcp') {
      const trust = formData.get('trust' + slot) === 'on';
      let pool_pubkey = null;
      if (!trust) {
        const hex = (formData.get('key' + slot) || '').trim();
        if (!/^[0-9a-fA-F]{64}$/.test(hex)) {
          msg.style.color = 'var(--red)';
          msg.textContent = 'Pool ' + slot + ': pubkey must be 64 hex chars';
          return;
        }
        pool_pubkey = hexToBytes(hex);
      }
      protocol = { Sv2: { pool_pubkey } };
    } else {
      const pass = formData.get('pass' + slot);
      protocol = { Sv1: { password: pass || null } };
    }
    pool.push({ host, port, user, protocol });
  }
  try {
    await postWithRetry(() => csrfPost({ pool }));
    msg.style.color = '';
    msg.textContent = 'Saved.';
    loadPoolMeta();
  } catch (error) {
    msg.style.color = 'var(--red)';
    msg.textContent = 'Save failed: ' + error.message;
  }
});

$('fmTemp').addEventListener('submit', async event => {
  event.preventDefault();
  const msg = $('msgTemp');
  msg.style.color = '';
  const dangerC = +$('dangerC').value;
  const cooldownC = +$('cooldownC').value;
  const targetC = +$('targetC').value;
  const body = {};
  // sample_interval_ms is not user-editable but the API expects a
  // complete thermal object, so replay the loaded value alongside any
  // edited threshold.
  if (!tempInit || dangerC !== tempInit.dangerC || cooldownC !== tempInit.cooldownC) {
    body.thermal = {
      sample_interval_ms: tempInit ? tempInit.sampleMs : null,
      dangerous_temp_c: dangerC,
      cooldown_temp_c: cooldownC,
    };
  }
  if (!tempInit || targetC !== tempInit.targetC) {
    body.cooling_target_temp_c = targetC;
  }
  if (!Object.keys(body).length) {
    msg.textContent = 'No changes.';
    return;
  }
  try {
    await postWithRetry(() => csrfPost(body));
    msg.style.color = '';
    msg.textContent = 'Saved.';
    tempInit = { sampleMs: tempInit ? tempInit.sampleMs : null, dangerC, cooldownC, targetC };
  } catch (error) {
    msg.style.color = 'var(--red)';
    msg.textContent = 'Save failed: ' + error.message;
  }
});

$('fmMining').addEventListener('submit', async event => {
  event.preventDefault();
  const msg = $('msgMining');
  msg.style.color = '';
  try {
    await postWithRetry(() => csrfPost({
      tuning: {
        frequency_mhz: +$('freqMhz').value,
        voltage_mv: +$('voltMv').value,
      },
      miner: {
        algorithm: $('tunerAlgo').value,
      },
    }));
    msg.style.color = '';
    msg.textContent = 'Saved.';
  } catch (error) {
    msg.style.color = 'var(--red)';
    msg.textContent = 'Save failed: ' + error.message;
  }
});

$('fmSystem').addEventListener('submit', async event => {
  event.preventDefault();
  const msg = $('msgSystem');
  msg.style.color = '';
  try {
    await postWithRetry(() => csrfPost({
      api: {
        username: $('sysUser').value,
        password: $('sysPass').value,
      },
    }));
    msg.style.color = '';
    msg.textContent = 'Saved.';
  } catch (error) {
    msg.style.color = 'var(--red)';
    msg.textContent = 'Save failed: ' + error.message;
  }
});

// Fetch GET /api/system/network to populate the live address fields.
// Both DHCP and Static modes return real values from the embassy-net
// stack (DHCP returns the lease). Retries with backoff to tolerate a
// concurrent /api/info poll occupying the REST listener's workers.
async function loadNetworkStatus() {
  // Skip if a load is already in flight so rapid tab switches cannot race
  // two runs and apply stale responses out of order.
  if (networkStatusLoading) return;
  networkStatusLoading = true;
  const delays = [0, 250, 500, 1000];
  let lastError;
  try {
    for (const wait of delays) {
      if (wait) await new Promise(resolve => setTimeout(resolve, wait));
      try {
        const response = await fetch(API + '/api/system/network', { cache: 'no-store', credentials: 'include' });
        if (!response.ok) throw new Error('HTTP ' + response.status);
        const data = await response.json();
        // Set the mode from the live status so the segmented buttons reflect
        // the board's current mode even before loadCfg completes.
        const modeVal = data.mode === 'static' ? 'Static' : 'Dhcp';
        $('netMode').value = modeVal;
        if (data.address) $('netAddr').value = bytesToIp(data.address);
        if (data.prefix_len != null) $('netMask').value = prefixToMask(data.prefix_len);
        if (data.gateway) $('netGw').value = bytesToIp(data.gateway);
        const dns = data.dns || [];
        $('netDns1').value = dns[0] ? bytesToIp(dns[0]) : '';
        $('netDns2').value = dns[1] ? bytesToIp(dns[1]) : '';
        syncNetUi();
        return;
      } catch (error) {
        lastError = error;
        console.warn('loadNetworkStatus attempt failed:', error);
      }
    }
    // Non-fatal: fields stay as-is after all retries fail.
    console.warn('loadNetworkStatus: all retries exhausted:', lastError);
  } finally {
    networkStatusLoading = false;
  }
}

$('fmNetwork').addEventListener('submit', event => {
  event.preventDefault();
  const msg = $('msgNetwork');
  msg.style.color = '';
  msg.textContent = '';
  const modeVal = $('netMode').value;
  const hostname = $('netHostname').value;
  // Mirror the server's hostname rule so the apply, which is fired without
  // awaiting a response, cannot be silently rejected: letters, digits, and
  // hyphens only, with no leading or trailing hyphen. Empty is allowed.
  if (hostname && (!/^[a-zA-Z0-9-]+$/.test(hostname) || hostname.startsWith('-') || hostname.endsWith('-'))) {
    msg.style.color = 'var(--red)';
    msg.textContent = 'Hostname may use only letters, digits, and hyphens, not at the ends.';
    return;
  }
  let mode;
  let summary;
  if (modeVal === 'Dhcp') {
    mode = 'Dhcp';
    summary = 'Switch to DHCP';
  } else {
    const addrStr = $('netAddr').value.trim();
    const gwStr = $('netGw').value.trim();
    const maskStr = $('netMask').value.trim();
    const address = ipToBytes(addrStr);
    if (!address) {
      msg.style.color = 'var(--red)';
      msg.textContent = 'Invalid address.';
      return;
    }
    const gateway = ipToBytes(gwStr);
    if (!gateway) {
      msg.style.color = 'var(--red)';
      msg.textContent = 'Invalid gateway.';
      return;
    }
    const prefix_len = maskToPrefix(maskStr);
    if (prefix_len === null) {
      msg.style.color = 'var(--red)';
      msg.textContent = 'Invalid mask.';
      return;
    }
    const dns = [];
    for (const id of ['netDns1', 'netDns2']) {
      const value = $(id).value.trim();
      if (!value) continue;
      const parsed = ipToBytes(value);
      if (!parsed) {
        msg.style.color = 'var(--red)';
        msg.textContent = 'Invalid DNS address in ' + id + '.';
        return;
      }
      dns.push(parsed);
    }
    mode = { Static: { address, prefix_len, gateway, dns } };
    summary = 'Switch to static ' + addrStr + '/' + prefix_len + ' gw ' + gwStr;
  }
  pendingNetSwitch = { mode, hostname };
  $('netModalSummary').textContent = summary;
  // Default the confirm-or-revert window on for every mode: it is the safe
  // choice (a change that strands the board auto-reverts). Static changes
  // must keep it: a wrong static address strands the board and persists
  // immediately with no window, and a reboot cannot recover, so lock the box
  // on for static. DHCP self-heals on reboot, so it stays optional there and
  // the reconnect-and-confirm step still reaches the board over its mDNS
  // hostname even when the new lease address is unknown.
  $('netModalConfirmAfter').checked = true;
  $('netModalConfirmAfter').disabled = modeVal === 'Static';
  $('netModalWarn').classList.add('hide');
  $('msgNetModal').textContent = '';
  showModal();
});

$('netModalConfirmAfter').addEventListener('change', function () {
  $('netModalWarn').classList.toggle('hide', this.checked);
});

$('btnNetModalCancel').addEventListener('click', () => {
  $('netModal').classList.add('hide');
  pendingNetSwitch = null;
});

// Show the terminal "configuration changed" notice after a network apply.
// The change drops this connection and the firmware has already
// invalidated the session, so navigating anywhere just chases an address
// that is mid-reconfigure (or, for DHCP, unknown) and lands on a browser
// error. Instead, stop polling and show a static notice telling the
// operator to reconnect and log in once the board is back; for a static
// address the new location is known, so offer it as a link.
function showSwitchedNotice(mode, hostname, confirmAfterSwitch) {
  clearInterval(pollTimer);
  $('netModal').classList.add('hide');
  const text = $('netChangedText');
  if (mode.Static) {
    const url = location.protocol + '//' + bytesToIp(mode.Static.address) + '/';
    text.innerHTML = 'The board is now at <a href="' + url + '">' + url + '</a>.';
  } else {
    text.textContent = 'The board is switching to a new DHCP address.';
  }
  // The inbound HTTP listener takes up to ~30 s to start accepting again
  // after the live reconfiguration, so set that expectation rather than
  // letting an early reconnect look like a failure. See docs/HTTP_LISTENERS.md.
  text.append(' It can take up to 30 seconds to become reachable, then log in again.');
  if (confirmAfterSwitch) {
    text.append(' Log in and confirm within 2 minutes, or the change reverts.');
  }
  // The IP change is live, but the mDNS responder only reads the hostname at
  // boot, so a renamed board keeps answering at its old .local name until a
  // reboot. Say so rather than let the new name look broken.
  if (hostname && hostname !== appliedHostname) {
    text.append(' The new name ' + hostname + '.local takes effect after a reboot.');
  }
  $('netChanged').classList.remove('hide');
}

$('btnNetModalConfirm').addEventListener('click', async () => {
  if (!pendingNetSwitch || applyInFlight) return;
  const { mode, hostname } = pendingNetSwitch;
  const confirmAfterSwitch = $('netModalConfirmAfter').checked;
  pendingNetSwitch = null;
  applyInFlight = true;
  const msg = $('msgNetwork');
  // Pause the 2 s poll so it does not contend for an :8080 listener
  // worker with the apply request; resume it if the apply fails.
  clearInterval(pollTimer);
  const fail = text => {
    applyInFlight = false;
    pollTimer = setInterval(tick, 2000);
    $('netModal').classList.add('hide');
    msg.style.color = 'var(--red)';
    msg.textContent = text;
  };
  let csrf;
  try {
    csrf = await csrfToken();
  } catch (error) {
    fail('Could not get CSRF token: ' + ((error && error.message) || 'network error'));
    return;
  }
  // Wait for the 200 ack that confirms the command reached the board. The
  // firmware holds off the switch so this ack flushes before the
  // connection drops, so a thrown fetch means the command did not arrive
  // (not that the switch happened); report it and let the operator retry
  // rather than show a false success. See docs/HTTP_LISTENERS.md.
  let response;
  try {
    response = await csrfPostTo('/api/system/network', { mode, hostname, confirm_after_switch: confirmAfterSwitch }, csrf);
  } catch (_) {
    fail('Could not reach the board to apply the change. Try again.');
    return;
  }
  if (!response.ok) {
    const errBody = await response.json().catch(() => ({}));
    fail(errBody.message || ('HTTP ' + response.status));
    return;
  }
  showSwitchedNotice(mode, hostname, confirmAfterSwitch);
});

// Confirm or revert the pending change from the modal. Neither POST
// changes the IP, so retry with backoff against a transient failure on
// the freshly reconnected link. Closes the modal on success.
async function netModalPost(path, errPrefix) {
  const msg = $('msgNetModal');
  msg.style.color = '';
  msg.textContent = '';
  // Guard against a double-click spawning a second concurrent POST that
  // would also mutate the message and close the modal.
  const buttons = [$('btnNetModalPersist'), $('btnNetModalRevert')];
  buttons.forEach(button => { button.disabled = true; });
  try {
    await postWithRetry(() => csrfPostTo(path, {}));
    $('netModal').classList.add('hide');
  } catch (error) {
    msg.style.color = 'var(--red)';
    msg.textContent = errPrefix + ': ' + (error.message || 'unknown');
  } finally {
    buttons.forEach(button => { button.disabled = false; });
  }
}

$('btnNetModalPersist').addEventListener('click', () => netModalPost('/api/system/network/confirm', 'Confirm failed'));
$('btnNetModalRevert').addEventListener('click', () => netModalPost('/api/system/network/revert', 'Revert failed'));

// Factory reset reboots the board to DHCP, so the address is unknown and a
// poll would land on an error. Stop polling and show a static notice telling
// the operator to reconnect once the board is back, mirroring the network
// switch notice.
function showResetDoneNotice() {
  clearInterval(pollTimer);
  $('resetStep2').classList.add('hide');
  $('resetStep1').classList.remove('hide');
  $('resetDoneText').textContent =
    'The board has been reset and is rebooting to DHCP with default credentials.'
    + ' It can take up to 30 seconds to become reachable, then log in again.';
  $('resetDone').classList.remove('hide');
}

$('btnResetStart').addEventListener('click', () => {
  $('resetStep1').classList.add('hide');
  $('resetStep2').classList.remove('hide');
  $('msgReset').textContent = '';
});

$('btnResetCancel').addEventListener('click', () => {
  $('resetStep2').classList.add('hide');
  $('resetStep1').classList.remove('hide');
});

$('btnResetConfirm').addEventListener('click', async () => {
  const msg = $('msgReset');
  msg.style.color = '';
  msg.textContent = '';
  // Pause the 2 s poll so it does not contend for an :8080 listener
  // worker with the reset request; resume it if the reset fails.
  clearInterval(pollTimer);
  const fail = text => {
    pollTimer = setInterval(tick, 2000);
    msg.style.color = 'var(--red)';
    msg.textContent = text;
  };
  // Wait for the 200 ack that confirms the command reached the board. The
  // firmware flushes this ack before the wipe and reboot, so a thrown fetch
  // means the command did not arrive (not that the reset happened); report
  // it and let the operator retry rather than show a false success.
  let response;
  try {
    response = await csrfPostTo('/api/system/factory_reset', {});
  } catch (_) {
    fail('Could not reach the board to reset. Try again.');
    return;
  }
  if (!response.ok) {
    fail('Reset failed: HTTP ' + response.status);
    return;
  }
  showResetDoneNotice();
});
