// IPv4 helpers shared by the network config form. The API carries
// addresses as 4-element byte arrays and masks as prefix lengths.

export function ipToBytes(str: string): number[] | null {
  const parts = str.trim().split('.');
  if (parts.length !== 4) return null;
  const bytes = parts.map(Number);
  if (bytes.some((b) => !Number.isInteger(b) || b < 0 || b > 255)) return null;
  return bytes;
}

export const bytesToIp = (bytes: number[]): string => bytes.join('.');

export function prefixToMask(prefix: number): string {
  const bits = prefix >>> 0;
  const value = bits === 0 ? 0 : (0xffffffff << (32 - bits)) >>> 0;
  return [(value >>> 24) & 0xff, (value >>> 16) & 0xff, (value >>> 8) & 0xff, value & 0xff].join('.');
}

export function maskToPrefix(maskStr: string): number | null {
  const bytes = ipToBytes(maskStr);
  if (!bytes) return null;
  const value = ((bytes[0] << 24) | (bytes[1] << 16) | (bytes[2] << 8) | bytes[3]) >>> 0;
  const inverted = (~value) >>> 0;
  if ((inverted & (inverted + 1)) !== 0) return null;
  const leading = Math.clz32(~value >>> 0);
  if (leading < 1 || leading > 32) return null;
  return leading;
}
