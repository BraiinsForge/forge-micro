// Shared reactive dashboard state, driven by a single 2 s poll of
// /api/info. TopNav (action state), the Dashboard cards, the chart
// buffers, and the footer all read from here so there is one poll for
// the whole page.

import { createSignal } from './signal';
import { getInfo } from './api';
import type { Info } from './types';

export const SAMPLES = 150; // 150 × 2 s = 5-minute rolling window
const FAIL_THRESHOLD = 3; // consecutive misses before the link reads Dead

// A chart buffer holds one value per poll, with `null` for a missed or
// absent sample so the line breaks rather than interpolating over a gap.
type Sample = number | null;

const emptyBuf = (): Sample[] => new Array<Sample>(SAMPLES).fill(null);

export interface Dash {
  info: Info | null;
  link: boolean;
  // The chip temperature the page may show. A stale reading is a held or
  // zero value the board did not measure, so it is absent here, and the
  // card, its colour and the chart all read this one field.
  temperature: number | null;
  hr: Sample[];
  tp: Sample[];
  pin: Sample[];
  pout: Sample[];
}

export const dash: Dash = {
  info: null,
  link: false,
  temperature: null,
  hr: emptyBuf(),
  tp: emptyBuf(),
  pin: emptyBuf(),
  pout: emptyBuf(),
};

const sig = createSignal(dash);
export const useDash = sig.useValue;

function push(buf: Sample[], value: Sample): void {
  buf.shift();
  buf.push(value);
}

let failures = 0;
let timer: ReturnType<typeof setInterval> | null = null;

async function tick(): Promise<void> {
  try {
    const data = await getInfo();
    failures = 0;
    dash.link = true;
    dash.info = data;
    push(dash.hr, data.hashrate ?? null);
    dash.temperature = data.chip_reading_stale || data.temperature == null ? null : data.temperature;
    push(dash.tp, dash.temperature);
    push(dash.pin, data.power_in_mw == null ? null : data.power_in_mw / 1000);
    push(dash.pout, data.power_out_mw == null ? null : data.power_out_mw / 1000);
  } catch (error) {
    failures++;
    if (failures >= FAIL_THRESHOLD) dash.link = false;
  }
  sig.notify();
}

export function startPoll(): void {
  if (timer) return;
  tick();
  timer = setInterval(tick, 2000);
}

export function stopPoll(): void {
  if (timer) clearInterval(timer);
  timer = null;
}
