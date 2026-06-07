// Dashboard SPA. Polls /api/info every 2 s for the live tiles and the
// rolling charts, and reads /api/config on demand for the Configuration
// page. All `/api/*` calls go to port 8080 (ubos-api); the static
// shell, login, and logout live on the same port 80 we were served from.
const API = `${location.protocol}//${location.hostname}:8080`;

// Chart geometry. CHART_WIDTH and CHART_HEIGHT must match the SVG
// `viewBox` declared in index.html. The PLOT_* values are the inner
// drawing region after axis-label padding.
const SAMPLE_COUNT = 150;
const CHART_WIDTH = 680;
const CHART_HEIGHT = 120;
const PAD_LEFT = 52;
const PAD_RIGHT = 12;
const PAD_TOP = 10;
const PAD_BOTTOM = 20;
const PLOT_WIDTH = CHART_WIDTH - PAD_LEFT - PAD_RIGHT;
const PLOT_HEIGHT = CHART_HEIGHT - PAD_TOP - PAD_BOTTOM;

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
// sampleMs, dangerC, and cooldownC inputs from GET /api/system/thermal
// so the bounds live only in ubos-config::thermal.
async function loadThermalBounds() {
  if (thermalLoaded) return true;
  const response = await fetch(API + '/api/system/thermal', { cache: 'no-store', credentials: 'include' });
  if (!response.ok) throw new Error('HTTP ' + response.status);
  const data = await response.json();
  const bounds = data.bounds || {};
  const sampleInput = $('sampleMs');
  const dangerInput = $('dangerC');
  const cooldownInput = $('cooldownC');
  if (bounds.sampleIntervalMs) {
    sampleInput.min = bounds.sampleIntervalMs.start;
    sampleInput.max = bounds.sampleIntervalMs.end;
  }
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
};

const CFG_NAV = {
  pools: 'snPools',
  temperature: 'snTemp',
  mining: 'snMining',
  system: 'snSystem',
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

const charts = [
  {
    id: 'cHr',
    min: 0,
    max: 2e12,
    maj: 1e12,
    minTick: 5e11,
    axis: value => (value / 1e12).toFixed(0) + 'T',
    val: fmtHr,
    lines: [{ cls: 'hrL', buf: hrBuf, lbl: 'Hashrate' }],
  },
  {
    id: 'cTp',
    min: 20,
    max: 100,
    maj: 20,
    minTick: 10,
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
  const span = chart.max - chart.min;
  const steps = Math.round(span / chart.minTick);
  const majEvery = Math.round(chart.maj / chart.minTick);
  let gridSvg = '';
  let yAxisSvg = '';
  let xAxisSvg = '';
  for (let step = 0; step <= steps; step++) {
    const pixelY = PAD_TOP + PLOT_HEIGHT - (step / steps) * PLOT_HEIGHT;
    const isMajor = step % majEvery === 0;
    if (isMajor) {
      const value = chart.min + step * chart.minTick;
      gridSvg += `<line class="gr" x1="${PAD_LEFT}" y1="${pixelY}" x2="${PAD_LEFT + PLOT_WIDTH}" y2="${pixelY}"/>`;
      yAxisSvg += `<text class="ax" x="${PAD_LEFT - 6}" y="${pixelY + 3}" text-anchor="end">${chart.axis(value)}</text>`;
      continue;
    }
    yAxisSvg += `<line class="tk" x1="${PAD_LEFT - 3}" y1="${pixelY}" x2="${PAD_LEFT}" y2="${pixelY}"/>`;
  }
  for (let minute = 0; minute <= 5; minute++) {
    const pixelX = PAD_LEFT + (minute / 5) * PLOT_WIDTH;
    gridSvg += `<line class="gr" x1="${pixelX}" y1="${PAD_TOP}" x2="${pixelX}" y2="${PAD_TOP + PLOT_HEIGHT}"/>`;
    xAxisSvg += `<text class="ax" x="${pixelX}" y="${CHART_HEIGHT - 6}" text-anchor="middle">-${5 - minute}m</text>`;
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
      const pixelX = (PAD_LEFT + index * PLOT_WIDTH / (SAMPLE_COUNT - 1)).toFixed(1);
      const pixelY = (PAD_TOP + PLOT_HEIGHT - ((clamped - chart.min) / span) * PLOT_HEIGHT).toFixed(1);
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

async function tick() {
  try {
    const response = await fetch(API + '/api/info', { cache: 'no-store', credentials: 'include' });
    if (!response.ok) throw 0;
    const data = await response.json();
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
  } catch (error) {
    setLk(false);
    console.warn('tick fetch failed:', error);
  }
}
tick().then(loadPoolMeta);
setInterval(tick, 2000);

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
  // Same retry budget as loadCfg: the REST listener serves one
  // connection at a time, so a /api/info tick in flight can knock
  // this fetch over. A short backoff keeps the dashboard labels
  // populated without spinning when the board is genuinely offline.
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

function syncPoolUi(slot) {
  const root = $('pcfg' + slot);
  const form = $('cfgForm');
  const match = (form['url' + slot].value || '').match(POOL_RE);
  root.dataset.protocol = match && match[1] === 'stratum2+tcp' ? 'sv2' : 'sv1';
  root.dataset.trust = form['trust' + slot].checked ? 'yes' : 'no';
}

async function loadCfg() {
  // The REST listener serves connections one-at-a-time, so a click on
  // Configuration that lands while the 2 s /api/info poll is in flight
  // can fail at the TCP layer. Retry the GET a few times with backoff
  // before reporting Load failed, so the operator does not have to
  // bounce out of the tab to recover from a transient race.
  // Each section has its own .msg status div. Mirror load status to
  // all four so the operator sees the result on whichever section they
  // happen to be viewing when the load finishes.
  const msgIds = ['msg', 'msgTemp', 'msgMining', 'msgSystem'];
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
          $('sampleMs').value = data.thermal.sample_interval_ms;
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
        if (data.api) {
          $('sysUser').value = data.api.username || '';
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
    const rect = svg.getBoundingClientRect();
    const viewX = (event.clientX - rect.left) / rect.width * CHART_WIDTH;
    let index = Math.round((viewX - PAD_LEFT) * (SAMPLE_COUNT - 1) / PLOT_WIDTH);
    if (index < 0) index = 0;
    if (index >= SAMPLE_COUNT) index = SAMPLE_COUNT - 1;
    const pixelX = PAD_LEFT + index * PLOT_WIDTH / (SAMPLE_COUNT - 1);
    const secondsAgo = (SAMPLE_COUNT - 1 - index) * 2;
    const span = chart.max - chart.min;
    const circles = chart.lines.map(line => {
      const value = line.buf[index];
      if (value == null) return '';
      const clamped = Math.min(Math.max(value, chart.min), chart.max);
      const pixelY = PAD_TOP + PLOT_HEIGHT - ((clamped - chart.min) / span) * PLOT_HEIGHT;
      return `<circle class="${line.cls}" cx="${pixelX}" cy="${pixelY.toFixed(1)}" r="3"/>`;
    }).join('');
    tip.innerHTML = `<line class="cur" x1="${pixelX}" y1="${PAD_TOP}" x2="${pixelX}" y2="${PAD_TOP + PLOT_HEIGHT}"/>${circles}`;
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

$('nLogout').addEventListener('click', async event => {
  event.preventDefault();
  try {
    const tokenResponse = await fetch('/', { method: 'HEAD', cache: 'no-store' });
    const csrf = tokenResponse.headers.get('X-CSRF-Token') || '';
    await fetch('/logout', {
      method: 'POST',
      headers: { 'X-CSRF-Token': csrf },
      credentials: 'include',
      redirect: 'manual',
    });
  } catch (_) {
    // Navigate to /login even if the POST failed; the cookie is also
    // invalidated server-side on the next protected request.
  }
  location.href = '/login';
});

async function csrfPost(body) {
  const tokenResponse = await fetch('/', { method: 'HEAD', cache: 'no-store' });
  const csrf = tokenResponse.headers.get('X-CSRF-Token') || '';
  return fetch(API + '/api/config', {
    method: 'POST',
    headers: {
      'Content-Type': 'application/json',
      'X-CSRF-Token': csrf,
    },
    body: JSON.stringify(body),
    credentials: 'include',
  });
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
    const response = await csrfPost({ pool });
    if (response.ok) {
      msg.style.color = '';
      msg.textContent = 'Saved.';
      loadPoolMeta();
      return;
    }
    msg.style.color = 'var(--red)';
    msg.textContent = 'Save failed: HTTP ' + response.status;
  } catch (error) {
    msg.style.color = 'var(--red)';
    msg.textContent = 'Save failed: ' + error.message;
  }
});

$('fmTemp').addEventListener('submit', async event => {
  event.preventDefault();
  const msg = $('msgTemp');
  msg.style.color = '';
  const sampleMs = +$('sampleMs').value;
  const dangerC = +$('dangerC').value;
  const cooldownC = +$('cooldownC').value;
  const targetC = +$('targetC').value;
  const body = {};
  if (!tempInit || sampleMs !== tempInit.sampleMs || dangerC !== tempInit.dangerC || cooldownC !== tempInit.cooldownC) {
    body.thermal = {
      sample_interval_ms: sampleMs,
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
    const response = await csrfPost(body);
    if (response.ok) {
      msg.style.color = '';
      msg.textContent = 'Saved.';
      tempInit = { sampleMs, dangerC, cooldownC, targetC };
      return;
    }
    msg.style.color = 'var(--red)';
    msg.textContent = 'Save failed: HTTP ' + response.status;
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
    const response = await csrfPost({
      tuning: {
        frequency_mhz: +$('freqMhz').value,
        voltage_mv: +$('voltMv').value,
      },
    });
    if (response.ok) {
      msg.style.color = '';
      msg.textContent = 'Saved.';
      return;
    }
    msg.style.color = 'var(--red)';
    msg.textContent = 'Save failed: HTTP ' + response.status;
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
    const response = await csrfPost({
      api: {
        username: $('sysUser').value,
        password: $('sysPass').value,
      },
    });
    if (response.ok) {
      msg.style.color = '';
      msg.textContent = 'Saved.';
      return;
    }
    msg.style.color = 'var(--red)';
    msg.textContent = 'Save failed: HTTP ' + response.status;
  } catch (error) {
    msg.style.color = 'var(--red)';
    msg.textContent = 'Save failed: ' + error.message;
  }
});
