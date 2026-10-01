// Shared domain types for the board API. These mirror the JSON shapes
// the REST listener returns (the `ubos-api` crate) and the mock
// fixtures in `dev/mock.ts`. Fields the board may leave out of a given
// response are optional here, so a partial poll still type-checks and
// the components guard with `?.` / `??` rather than trusting presence.

/** OTA slot state, nested in the `GET /api/info` body. */
export interface Ota {
  runningSlot: string;
  runningVersion: number;
  /** The build id stamped into the running image; `null` where the board stamps none. */
  runningBuild?: string | null;
  maxImageBytes: number;
}

/** One hashboard chain slot, nested in the `GET /api/info` body. */
export interface ChainInfo {
  chain: number;
  state: 'present' | 'absent' | 'faulted' | 'unknown';
  chips: number;
  frequency_mhz: number;
  hashrate: number;
  hottest_c: number | null;
}

/** `GET /api/info` — the body the dashboard polls every two seconds. */
export interface Info {
  name: string;
  hashrate?: number;
  uptime?: number;
  temperature?: number;
  /** Ambient temperature near the board. `null` on a board without a sensor for it. */
  ambient?: number | null;
  /** `true` when `temperature` is a held-over value, not a fresh read. */
  chip_reading_stale?: boolean;
  fan_rpm?: number;
  fan_pwm?: number;
  power_in_mw?: number;
  power_out_mw?: number;
  psu_voltage_set_mv?: number;
  psu_voltage_measured_mv?: number;
  /** Sum of chip core voltages; not a PSU output-rail measurement. */
  chain_voltage_mv?: number;
  psu_fault?: boolean;
  /** Bitmask of live pool slots: bit N set means slot N is alive. */
  pools?: number;
  net_confirm_seconds?: number;
  running?: boolean;
  paused?: boolean;
  /** The manager's dominant reason for the halt; `null` when running. */
  pause_reason?: PauseReason | null;
  ip?: string;
  mac?: string;
  time?: string;
  ota?: Ota;
  chains: ChainInfo[];
}

// The reasons the board halts mining for, as the API spells them. The
// board treats these strings as an enum and the manager's
// `PauseReason::as_api_str` is their source; a rename there is a schema
// change. Every reader of `pause_reason` types against this union and
// takes its label from the map below, so a misspelling on this page is
// a type error rather than a control that silently never enables. The
// guard is one-sided: it covers what this page writes, not what the
// board sends, so a reader that displays a reason still falls back for
// one the map lacks.
export type PauseReason =
  | 'dangerousTemperature'
  | 'temperatureUnreadable'
  | 'brokenFan'
  | 'poolUnreachable'
  | 'otaUpdate'
  | 'tunerFault'
  | 'userRequest';

export const PAUSE_LABEL: Record<PauseReason, string> = {
  dangerousTemperature: 'Stopped - Overheating',
  temperatureUnreadable: 'Stopped - No temperature reading',
  brokenFan: 'Stopped - Fan fault',
  poolUnreachable: 'Stopped - Pools down',
  otaUpdate: 'Stopped - Updating',
  tunerFault: 'Stopped - Tuner fault',
  userRequest: 'Stopped - User',
};

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

// The fan runs the PID against the target under `auto` and holds the
// fixed duty under `fixed`, the two wire names of the config enum.
export type FanMode = 'auto' | 'fixed';

export interface CoolingConfig {
  mode: FanMode;
  target_temp_c: number;
  fixed_fan_pwm: number;
  min_fan_pwm: number;
}

export interface FixedTarget {
  frequency_mhz: number;
  voltage_mv: number;
}

export interface PowerTarget {
  input_power_mw: number;
  efficiency_mj_per_th: number;
}

// Externally tagged: exactly one key is present.
export interface Tuning {
  Fixed?: FixedTarget;
  Power?: PowerTarget;
}

export interface MinerConfig {
  tuning: Tuning;
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
  miner: MinerConfig;
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
  minFanPwm: Range;
  fixedFanPwm: Range;
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
