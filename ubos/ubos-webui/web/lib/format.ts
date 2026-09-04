// Value formatters shared across the dashboard. Ported from the
// vanilla ubos-webui so the two UIs render identical strings.

// The telemetry values arrive as `number | undefined` from a partial
// poll and are widened to allow `null` so callers can pass a cleared
// value straight through to the em-dash placeholder.
type Maybe = number | null | undefined;

export function fmtHr(hashes: Maybe): string {
  if (hashes == null) return '–';
  if (hashes >= 1e12) return (hashes / 1e12).toFixed(2) + ' TH/s';
  if (hashes >= 1e9) return (hashes / 1e9).toFixed(2) + ' GH/s';
  if (hashes >= 1e6) return (hashes / 1e6).toFixed(2) + ' MH/s';
  if (hashes >= 1e3) return (hashes / 1e3).toFixed(2) + ' kH/s';
  return hashes.toFixed(0) + ' H/s';
}

export function fmtUp(seconds: Maybe): string {
  if (seconds == null) return '–';
  const days = (seconds / 86400) | 0;
  const hours = ((seconds / 3600) % 24) | 0;
  const minutes = ((seconds / 60) % 60) | 0;
  if (days) return `${days}d ${hours}h ${minutes}m`;
  if (hours) return `${hours}h ${minutes}m`;
  return `${minutes}m ${(seconds % 60) | 0}s`;
}

// Describe a config field's accepted range for the row description in
// the settings forms. The bounds endpoints return an empty object until
// they answer, so a missing endpoint yields undefined and the caller
// renders no description at all rather than a half-filled one.
export function fmtRange(min: Maybe, max: Maybe): string | undefined {
  if (min == null || max == null) return undefined;
  return `Min: ${min} - Max: ${max}`;
}

export const fmtPower = (mw: Maybe): string => (mw ? (mw / 1000).toFixed(0) + ' W' : '–');
export const fmtVolts = (mv: Maybe): string => (mv ? parseFloat((mv / 1000).toFixed(3)) + ' V' : '–');

// Round a positive number up to the next 1, 2, or 5 times a power of
// ten so gridline intervals stay readable at any scale.
export function niceStep(raw: number): number {
  const pow = Math.pow(10, Math.floor(Math.log10(raw)));
  const norm = raw / pow;
  const mult = norm <= 1 ? 1 : norm <= 2 ? 2 : norm <= 5 ? 5 : 10;
  return mult * pow;
}
