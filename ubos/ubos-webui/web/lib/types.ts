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

/** One hashboard chain slot, nested in the `GET /api/info` body. */
export interface ChainInfo {
  chain: number;
  state: 'present' | 'absent' | 'faulted' | 'unknown';
  chips: number;
  frequency_khz: number;
  hashrate: number;
  hottest_c: number | null;
  /** The pool slot whose work the chain was last given; `null` when it has none. */
  pool?: number | null;
}

/** `GET /api/info` — the body the dashboard polls every two seconds. */
export interface Info {
  name: string;
  hashrate?: number;
  uptime?: number;
  /**
   * Chip temperature in °C. Read `chip_reading_stale` before showing
   * it: this field holds its last value when the board stops measuring.
   * `lib/store.ts` filters it into `dash.temperature`, and the card,
   * its colour and the chart all read that field instead of this one.
   */
  temperature?: number;
  /** Ambient temperature near the board. `null` on a board without a sensor for it. */
  ambient?: number | null;
  /** `true` when `temperature` is a held or zero value the board did not measure, not a fresh read. */
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
  /** The miner's lifecycle state; `null` before its first publish. */
  mining_state?: MiningState | null;
  paused?: boolean;
  /** The manager's dominant reason for the halt; `null` when running. */
  pause_reason?: PauseReason | null;
  ip?: string;
  mac?: string;
  time?: string;
  /** The build id the board stamped: the tip it was built from and the build time. */
  build?: string;
  /** The machine's name as its maker gives it. `null` on a board that does not report one. */
  product_name?: string | null;
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
  | 'otaUpdate'
  | 'tunerFault'
  | 'userRequest';

export const PAUSE_LABEL: Record<PauseReason, string> = {
  dangerousTemperature: 'Stopped - Overheating',
  temperatureUnreadable: 'Stopped - No temperature reading',
  brokenFan: 'Stopped - Fan fault',
  otaUpdate: 'Stopped - Updating',
  tunerFault: 'Stopped - Tuner fault',
  userRequest: 'Stopped - User',
};

// The miner's lifecycle states, as the API spells them. The board's
// `MiningState::as_api_str` is their source, so a rename there is a
// schema change. The fallback for a state this page lacks is a plain
// stop, the same way `PAUSE_LABEL` falls back.
export type MiningState = 'waitingForPool' | 'starting' | 'running' | 'stopped';

export const MINING_STATE_LABEL: Record<MiningState, string> = {
  waitingForPool: 'Waiting for pool',
  starting: 'Starting',
  running: 'Running',
  stopped: 'Stopped',
};

// The status class each state renders with. Waiting for a pool and
// bring-up are warnings rather than stops: the board is up and the
// chips are not hashing yet.
export const MINING_STATE_CLASS: Record<MiningState, string> = {
  waitingForPool: 'warn',
  starting: 'warn',
  running: 'running',
  stopped: 'stopped',
};

// A pool's stratum protocol, a single-key tagged union in the JSON. The
// board carries the Sv2 pubkey as a base58check string over JSON and
// accepts a null Sv1 password, so both inner fields are nullable. A null
// pubkey means the operator chose to skip the authority check.
export type PoolProtocol =
  | { Sv1: { password: string | null } }
  | { Sv2: { pool_pubkey: string | null } };

export interface PoolConfig {
  host: string;
  port: number;
  user: string;
  protocol?: PoolProtocol;
  weight?: number;
}

// The two dangerous thresholds are both halt thresholds, and the board
// judges against the one that matches the sensor its safety reading came
// from: the estimate where a PCB sensor plus an offset stands in for the
// die, the chip one where a die reading is real.
export interface ThermalConfig {
  sample_interval_ms: number;
  dangerous_estimated_temp_c: number;
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
  /** Target clock in kHz. The PLL grid steps by a fraction of a megahertz. */
  frequency_khz: number;
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

// One tuner the machine offers, externally tagged by the tuning it
// reads: exactly one key is present.
export interface TunerInfo {
  Fixed?: { name: string };
  Power?: { name: string; readsEfficiency: boolean };
}

export interface AsicBounds {
  frequencyKhz: Range;
  voltageMv: Range;
  /** Absent when no offered tuner reads a power target. */
  inputPowerMw: Range | null;
  /** The tuners the machine offers, in the order the form shows them. */
  tuners: TunerInfo[];
}

// The tuning a new config starts on: the board's fixed operating
// point, or a power target.
export type DefaultTuning = 'Fixed' | { Power: { inputPowerMw: number } };

/** The board's defaults, as `/api/system/asic` reports them. */
export interface AsicDefaults {
  defaultFrequencyKhz?: number;
  defaultVoltageMv?: number;
  defaultEfficiencyMjPerTh?: number;
  defaultTuning?: DefaultTuning;
}

export interface ThermalBounds {
  dangerousEstimatedTempC: Range;
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
