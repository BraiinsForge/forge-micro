// Shared reactive dashboard state, driven by a single 2 s poll of
// /api/info and /api/system/temperature. TopNav (action state), the
// Dashboard cards, the chart buffers, and the footer all read from here
// so there is one poll for the whole page.

import { createSignal } from './signal';
import { getInfo, getTemperature } from './api';
import type { Info, Source, Statistic, Temperature } from './types';

export const SAMPLES = 150; // 150 × 2 s = 5-minute rolling window
const FAIL_THRESHOLD = 3; // consecutive misses before the link reads Dead

// A chart buffer holds one value per poll, with `null` for a missed or
// absent sample so the line breaks rather than interpolating over a gap.
type Sample = number | null;

const emptyBuf = (): Sample[] => new Array<Sample>(SAMPLES).fill(null);

/** The `/api/system/temperature` key of a kind's reading at a statistic. */
export type TempKey = `${Source}${Capitalize<Statistic>}`;
const TEMP_KEYS: TempKey[] = ['chipMax', 'chipMean', 'hashboardMax', 'hashboardMean'];

const CAPITALIZED: Record<Statistic, Capitalize<Statistic>> = { max: 'Max', mean: 'Mean' };

/** The key of `kind`'s reading at `statistic`. */
export const tempKey = (kind: Source, statistic: Statistic): TempKey => `${kind}${CAPITALIZED[statistic]}`;

export interface Dash {
  info: Info | null;
  temperature: Temperature | null;
  link: boolean;
  hr: Sample[];
  // One buffer for each kind and statistic, so the chart draws each kind
  // at the statistic the PID follows it at. The board sends `null` for a kind
  // that a sample did not read, so a stale kind breaks its line.
  temp: Record<TempKey, Sample[]>;
  pin: Sample[];
  pout: Sample[];
}

export const dash: Dash = {
  info: null,
  temperature: null,
  link: false,
  hr: emptyBuf(),
  temp: Object.fromEntries(TEMP_KEYS.map((key) => [key, emptyBuf()])) as Record<TempKey, Sample[]>,
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
    // One after the other, because the REST pool is often one connection
    // deep. A failure of either one is a miss.
    const data = await getInfo();
    const temperature = await getTemperature();
    failures = 0;
    dash.link = true;
    dash.info = data;
    dash.temperature = temperature;
    push(dash.hr, data.hashrate ?? null);
    for (const key of TEMP_KEYS) push(dash.temp[key], temperature[key]);
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
