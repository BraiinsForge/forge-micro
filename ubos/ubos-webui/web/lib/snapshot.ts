// Diagnostic snapshot: fetch every read-only endpoint into one JSON
// document for download. Sections are fetched one at a time (the REST
// pool is often one connection deep), each with its own backoff; a
// section that still fails is recorded under `errors` rather than
// aborting the whole snapshot.

import { API, RETRY_DELAYS, loginRedirect, sleep } from './api';

const SECTIONS: [string, string][] = [
  ['info', '/api/info'],
  ['config', '/api/config'],
  ['system_info', '/api/system/info'],
  ['asic', '/api/system/asic'],
  ['thermal', '/api/system/thermal'],
  ['temperature', '/api/system/temperature'],
  ['cooling', '/api/system/cooling'],
  ['statistics', '/api/system/statistics'],
  ['scoreboard', '/api/system/scoreboard'],
  ['network', '/api/system/network'],
  ['events', '/api/system/events'],
  ['events_catalog', '/api/system/events/catalog'],
];

// One parsed Prometheus exposition line: a recognised metric, or a line
// kept verbatim when it does not match.
type MetricEntry =
  | { name: string; value: string; labels?: string; timestamp_ms?: number }
  | { raw: string };

// One Prometheus exposition line. The label group alternates
// "outside a quoted value" and "a whole quoted value" so a literal '}'
// inside a value does not end the label block early.
const METRIC_LINE_RE =
  /^([A-Za-z_:][A-Za-z0-9_:]*)(?:\{((?:[^"}]|"(?:[^"\\]|\\.)*")*)\})?\s+(\S+)(?:\s+(\d+))?$/;

function parseMetrics(text: string): MetricEntry[] {
  const entries: MetricEntry[] = [];
  for (const line of text.split(/\r?\n/)) {
    if (!line || line.startsWith('#')) continue;
    const match = METRIC_LINE_RE.exec(line);
    if (match) {
      const entry: MetricEntry = { name: match[1], value: match[3] };
      if (match[2]) entry.labels = match[2];
      if (match[4]) entry.timestamp_ms = Number(match[4]);
      entries.push(entry);
    } else {
      entries.push({ raw: line });
    }
  }
  return entries;
}

async function fetchSectionWithRetry<T>(
  url: string,
  options: RequestInit,
  parse: (response: Response) => Promise<T>
): Promise<T | undefined> {
  let lastError: unknown;
  for (const wait of RETRY_DELAYS) {
    if (wait) await sleep(wait);
    let response: Response;
    try {
      response = await fetch(url, options);
    } catch (error) {
      lastError = error;
      continue;
    }
    if (response.status === 401) {
      loginRedirect();
      return undefined;
    }
    if (!response.ok) throw new Error('HTTP ' + response.status);
    return parse(response);
  }
  throw lastError;
}

export async function collectSnapshot(): Promise<Record<string, unknown>> {
  const snapshot: Record<string, unknown> = {
    version: 1,
    meta: {
      generated_at: new Date().toISOString(),
      hostname: location.hostname,
      page_url: location.href,
      user_agent: navigator.userAgent,
    },
  };
  const jobs: [string, () => Promise<unknown>][] = SECTIONS.map(([key, path]) => [
    key,
    () => fetchSectionWithRetry(API + path, { cache: 'no-store' }, (r) => r.json()),
  ]);
  // The metrics listener answers with a wildcard CORS header, which
  // browsers reject for credentialed requests, so omit credentials.
  jobs.push([
    'metrics',
    () =>
      fetchSectionWithRetry(
        `${location.protocol}//${location.hostname}:8081/metrics`,
        { cache: 'no-store' },
        (r) => r.text().then(parseMetrics)
      ),
  ]);
  const errors: Record<string, string> = {};
  for (const [key, run] of jobs) {
    try {
      snapshot[key] = await run();
    } catch (error) {
      errors[key] = String((error && (error as Error).message) || error);
    }
  }
  snapshot.errors = errors;
  return snapshot;
}
