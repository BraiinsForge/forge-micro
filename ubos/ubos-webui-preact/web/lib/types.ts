// Shared domain types for the board API. These mirror the JSON shapes
// the REST listener returns (the `ubos-api` crate) and the mock
// fixtures in `dev/mock.ts`. Fields the board may leave out of a given
// response are optional here, so a partial poll still type-checks and
// the components guard with `?.` / `??` rather than trusting presence.

/** OTA slot state, nested in the `GET /api/info` body. */
export interface Ota {
  runningSlot: string;
  runningVersion: number;
  maxImageBytes: number;
}

/** `GET /api/info` — the body the dashboard polls every two seconds. */
export interface Info {
  name: string;
  hashrate?: number;
  uptime?: number;
  temperature?: number;
  fan_rpm?: number;
  fan_pwm?: number;
  power_in_mw?: number;
  power_out_mw?: number;
  voltage_set_mv?: number;
  voltage_measured_mv?: number;
  psu_fault?: boolean;
  /** Bitmask of live pool slots: bit N set means slot N is alive. */
  pools?: number;
  net_confirm_seconds?: number;
  running?: boolean;
  paused?: boolean;
  pause_reason?: string | null;
  ip?: string;
  mac?: string;
  time?: string;
  ota?: Ota;
}

// A pool's stratum protocol, a single-key tagged union in the JSON. The
// board carries the Sv2 pubkey as raw bytes and accepts a null Sv1
// password, so both inner fields are nullable.
export type PoolProtocol =
  | { Sv1: { password: string | null } }
  | { Sv2: { pool_pubkey: number[] | null } };

export interface PoolConfig {
  host: string;
  port: number;
  user: string;
  protocol?: PoolProtocol;
  weight?: number;
}

export interface ThermalConfig {
  sample_interval_ms: number;
  dangerous_temp_c: number;
  cooldown_temp_c: number;
  dangerous_chip_temp_c: number;
}

export interface CoolingConfig {
  target_temp_c: number;
  min_fan_pwm: number;
}

export interface TuningConfig {
  frequency_mhz: number;
  voltage_mv: number;
}

export interface ApiCredentials {
  username: string;
  password: string;
}

export interface NetworkConfig {
  mode: string;
  hostname?: string;
  address: number[];
  prefix_len: number;
  gateway: number[];
  dns: number[][];
}

/** `GET /api/config`. */
export interface Config {
  pool: (PoolConfig | null)[];
  thermal: ThermalConfig;
  cooling: CoolingConfig;
  tuning: TuningConfig;
  api: ApiCredentials;
  network: NetworkConfig;
}

// A config write is a partial document: each settings tab POSTs only
// the slice it owns. The board merges it, so the values are opaque here.
export type ConfigPatch = Partial<Record<keyof Config, unknown>>;

/** An inclusive numeric range, the shape every bounds endpoint uses. */
export interface Range {
  start: number;
  end: number;
}

export interface AsicBounds {
  frequencyMhz: Range;
  voltageMv: Range;
}

export interface ThermalBounds {
  dangerousTempC: Range;
  cooldownTempC: Range;
  dangerousChipTempC: Range;
}

export interface CoolingBounds {
  targetTempC: Range;
}

export type EventSeverity = 'info' | 'warn' | 'error';

export interface EventCatalogEntry {
  code: number;
  fmt: string;
  args: string[];
}

export interface EventEntry {
  seq: number;
  code: number;
  args: string;
  sev: EventSeverity;
  ts: number;
}

export interface EventsResponse {
  events: EventEntry[];
}

export type ToastKind = 'success' | 'error';

export interface Toast {
  id: number;
  kind: ToastKind;
  title: string;
  text?: string;
}

// A pending network change, staged by the network form and read by the
// confirm/revert overlay. `mode` is DHCP (a bare string) or a static
// assignment carrying the full IPv4 configuration.
export type NetMode =
  | 'Dhcp'
  | { Static: { address: number[]; prefix_len: number; gateway: number[]; dns: number[][] } };

export interface NetChange {
  mode: NetMode;
  hostname?: string;
  appliedHostname?: string;
  confirmAfter?: boolean;
}
