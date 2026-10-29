import { useEffect, useState } from 'preact/hooks';
import type { ComponentChildren } from 'preact';
import Chart from './Chart';
import { useDash } from '../lib/store';
import { getConfig } from '../lib/api';
import { fmtHr, fmtUp, fmtPower, fmtVolts } from '../lib/format';
import { MC_PER_C, MINING_STATE_CLASS, MINING_STATE_LABEL, PAUSE_LABEL, SOURCE_LABEL, SOURCES } from '../lib/types';
import type { ChainInfo, CoolingConfig, Info, Source, Statistic } from '../lib/types';

// The colour of each kind's temperature line. The temperature card puts
// a dot of it before the reading the PID follows.
// Neither hue reads as hot or as a status, so a line never looks like
// the green or red that the card gives a reading.
const TEMP_COLOR: Record<Source, string> = { chip: '#d946ef', hashboard: '#0891b2' };

// A pool slot's display metadata, derived from GET /api/config. `null`
// means the slot answered but is unconfigured; `undefined` (the initial
// state) means the config fetch has not resolved yet.
interface PoolMeta {
  host: string;
  port: number;
  user: string;
  sv2: boolean;
}

interface Status {
  text: string;
  cls: string;
  // False while the state is not known yet: the text is then the same
  // placeholder the other empty cells show, and a dot would colour it.
  dot?: boolean;
}

function statusOf(info: Info | null): Status {
  if (!info || info.paused == null) return { text: '–', cls: '', dot: false };
  if (info.paused) {
    return {
      // The union covers the reasons this page knows. The string comes
      // from the board, and a firmware ahead of its bundled dashboard
      // sends one the map lacks, so that reads as a plain stop rather
      // than an empty status.
      text: info.pause_reason ? (PAUSE_LABEL[info.pause_reason] ?? 'Stopped') : 'Stopped',
      cls: info.pause_reason === 'userRequest' ? 'stopped' : 'warn',
    };
  }
  // The pause axis above is the operator's and the safety loops'; the
  // miner's own lifecycle below says where bring-up stands. A state
  // this page lacks reads as a plain stop, as `PAUSE_LABEL` does.
  const state = info.mining_state ?? 'stopped';
  return {
    text: MINING_STATE_LABEL[state] ?? MINING_STATE_LABEL.stopped,
    cls: MINING_STATE_CLASS[state] ?? MINING_STATE_CLASS.stopped,
  };
}

// The board reports one entry per chain it actually owns, so the card
// shows every entry the API returns.
function visibleChains(info: Info | null): ChainInfo[] {
  return info?.chains ?? [];
}

function chainStatus(state: ChainInfo['state']): Status {
  switch (state) {
    case 'present':
      return { cls: 'live', text: 'Alive' };
    case 'absent':
      return { cls: 'empty', text: 'Absent' };
    case 'faulted':
      return { cls: 'fault', text: 'Faulted' };
    default:
      return { cls: 'unknown', text: '—', dot: false };
  }
}

interface StatCardProps {
  label: ComponentChildren;
  value: ComponentChildren;
  cls?: string;
  wide?: boolean;
}

function StatCard({ label, value, cls, wide }: StatCardProps) {
  return (
    <div class={'card' + (wide ? ' wide' : '')}>
      <div class="k">{label}</div>
      <div class={'v' + (cls ? ' ' + cls : '')}>{value}</div>
    </div>
  );
}

// The board's halt thresholds, one for each kind of reading it measures.
// The temperature card turns red when the hottest reading of a kind
// reaches the threshold of that kind.
interface TempThresholds {
  hashboardC: number | null;
  chipC: number | null;
}

/** A threshold in degrees, or null where the machine has none. */
const celsius = (millicelsius: number | null) => (millicelsius === null ? null : millicelsius / MC_PER_C);

function StatusDot({ text, dot = true }: { text: string; dot?: boolean }) {
  return (
    <span class="ps">
      {dot && <span class="pd" />}
      {text}
    </span>
  );
}

export interface DashboardProps {
  shown: boolean;
}

export default function Dashboard({ shown }: DashboardProps) {
  const dash = useDash();
  const info = dash.info;
  const status = statusOf(info);
  const measuredVoltage = info?.psu_voltage_measured_mv || info?.chain_voltage_mv;

  const [poolCfg, setPoolCfg] = useState<(PoolMeta | null | undefined)[]>([undefined, undefined, undefined]);
  const [tempThresh, setTempThresh] = useState<TempThresholds | null>(null);
  const [cooling, setCooling] = useState<CoolingConfig | null>(null);
  const [cursor, setCursor] = useState<number | null>(null);

  // Re-read on every return to the view: the dashboard stays mounted while
  // hidden, so a config edit would otherwise show only after a reload.
  useEffect(() => {
    if (!shown) return;
    (async () => {
      try {
        const data = await getConfig();
        const pools = data.pool || [];
        setPoolCfg(
          [0, 1, 2].map((slot): PoolMeta | null => {
            const pool = pools[slot];
            if (!pool) return null;
            return {
              host: pool.host,
              port: pool.port,
              user: pool.user || '',
              sv2: !!(pool.protocol && 'Sv2' in pool.protocol),
            };
          })
        );
        if (data.thermal) {
          setTempThresh({
            hashboardC: celsius(data.thermal.dangerous_hashboard_temp),
            chipC: celsius(data.thermal.dangerous_chip_temp),
          });
        }
        if (data.cooling) setCooling(data.cooling);
      } catch (_) {
        /* leave labels blank; the poll still drives alive/dead */
      }
    })();
  }, [shown]);

  // The target's kind at its statistic, and the other kind at its
  // hottest, which the halt judges. So the card's reading sits on the line
  // of its dot's colour.
  const pidTarget = cooling?.target;
  const statisticOf = (kind: Source): Statistic => (pidTarget?.source === kind ? pidTarget.statistic : 'max');
  // One line for each kind with a reading in the window.
  const tempLines = SOURCES.map((kind) => ({ kind, buf: dash.temp[`temperature_${kind}_${statisticOf(kind)}`] }))
    .filter(({ buf }) => buf.some((value) => value != null))
    .map(({ kind, buf }) => ({
      buf,
      color: TEMP_COLOR[kind],
      label: `${SOURCE_LABEL[kind]} Temp. (°C)`,
      tip: `${SOURCE_LABEL[kind]} ${statisticOf(kind)} (°C)`,
    }));
  // The kind the PID follows, or the target's kind while the PID does not
  // run, which then has no target in force.
  const active = info?.pid_source;
  const cardKind = active ?? pidTarget?.source;
  const cardTemp = cardKind ? info?.[`temperature_${cardKind}_${statisticOf(cardKind)}`] : null;
  const cardTarget = active && pidTarget ? `${pidTarget.temp / MC_PER_C} °C` : '–';

  function tempCls(): string {
    if (cardTemp == null || !tempThresh) return '';
    const hot = (value: number | null | undefined, limit: number | null) =>
      value != null && limit !== null && value >= limit;
    return hot(info?.temperature_chip_max, tempThresh.chipC) ||
      hot(info?.temperature_hashboard_max, tempThresh.hashboardC)
      ? 'temp-red'
      : 'temp-ok';
  }

  function poolState(slot: number): Status {
    const mask = (info?.pools ?? 0) | 0;
    const alive = (mask >> slot) & 1;
    const meta = poolCfg[slot];
    if (alive && info?.running && info.chains?.some((chain) => chain.pool === slot)) {
      return { cls: 'live', text: 'Mining' };
    }
    if (alive) return { cls: 'live', text: 'Alive' };
    if (meta === null) return { cls: 'empty', text: 'Empty' };
    return { cls: '', text: 'Dead' };
  }

  return (
    <>
      <div class="left" onPointerLeave={() => setCursor(null)}>
        <Chart
          lines={[{ buf: dash.hr, color: '#6b50ff', label: 'Hashrate (TH/s)' }]}
          min={0}
          max={2e12}
          minSpan={2e11}
          axis={(v) => {
            const t = v / 1e12;
            return Number.isInteger(t) ? t.toFixed(0) : t.toFixed(1);
          }}
          val={fmtHr}
          cursor={cursor}
          onCursor={setCursor}
        />
        <Chart
          lines={tempLines}
          min={20}
          max={100}
          minSpan={20}
          axis={(v) => String(v)}
          val={(v) => v + ' °C'}
          cursor={cursor}
          onCursor={setCursor}
        />
        <Chart
          lines={[
            { buf: dash.pin, color: '#f9c74f', label: 'Power IN (W)' },
            { buf: dash.pout, color: '#4cc9f0', label: 'Power OUT (W)' },
          ]}
          min={0}
          max={60}
          minSpan={10}
          axis={(v) => String(v)}
          val={(v) => v.toFixed(0) + ' W'}
          cursor={cursor}
          onCursor={setCursor}
        />
      </div>

      <aside class="side">
        <StatCard label="Real Hashrate" value={fmtHr(info?.hashrate)} />
        <StatCard
          label="Temperature / Target"
          value={
            cardKind && cardTemp != null ? (
              <span class="ps">
                <span class="pd" style={{ background: TEMP_COLOR[cardKind] }} />
                <span class={tempCls()}>{cardTemp} °C</span> / {cardTarget}
              </span>
            ) : (
              '–'
            )
          }
        />
        <StatCard
          label="Power in / out"
          value={<><span>{fmtPower(info?.power_in_mw)}</span> / <span>{fmtPower(info?.power_out_mw)}</span></>}
        />
        <StatCard
          label="Voltage set / measured"
          value={<><span>{fmtVolts(info?.psu_voltage_set_mv)}</span> / <span>{fmtVolts(measuredVoltage)}</span></>}
        />
        <StatCard
          label="Fan speed / PWM"
          value={<><span>{info?.fan_rpm ? info.fan_rpm + ' RPM' : '–'}</span> / <span>{info?.fan_pwm ? info.fan_pwm + ' %' : '–'}</span></>}
        />
        <StatCard label="Uptime" value={fmtUp(info?.uptime)} />
        <div class={'card lkc' + (status.cls === 'running' ? ' live' : status.cls ? ' dead' : '')}>
          <div class="k">Status</div>
          <StatusDot text={status.text} dot={status.dot} />
        </div>
        <div class={'card lkc' + (dash.link ? ' live' : ' dead')}>
          <div class="k">Link</div>
          <StatusDot text={dash.link ? 'Alive' : 'Dead'} />
        </div>
        <StatCard label="Time UTC" value={info?.time ? info.time.replace(/ UTC$/, '') : '–'} />
        <StatCard label="Ambient Temperature" value={info?.ambient != null ? info.ambient + ' °C' : '–'} />
      </aside>

      {visibleChains(info).length > 0 && (
        <div class="card hbs">
          <div class="ph">
            <div class="k">Hashboards</div>
          </div>
          <div class="htr">
            <span>Chain</span>
            <span>Chips</span>
            <span>Frequency</span>
            <span>Hashrate</span>
            <span>Hottest</span>
            <span>Status</span>
          </div>
          {visibleChains(info).map((chain) => {
            const st = chainStatus(chain.state);
            const present = chain.state === 'present';
            return (
              <div class={'hb ' + st.cls} key={chain.chain}>
                <span>{chain.chain}</span>
                <span>{present ? chain.chips : '—'}</span>
                <span>{present ? chain.frequency_khz / 1000 + ' MHz' : '—'}</span>
                <span>{present ? fmtHr(chain.hashrate) : '—'}</span>
                <span>{chain.hottest_c == null ? '—' : chain.hottest_c + ' °C'}</span>
                <StatusDot text={st.text} dot={st.dot} />
              </div>
            );
          })}
        </div>
      )}

      <div class="card pools">
        <div class="ph">
          <div class="k">Pools</div>
          <a class="plink" href="#config/pools">Configure</a>
        </div>
        <div class="ptr">
          <span class="purl">URL</span>
          <span>Protocol</span>
          <span>User</span>
          <span>Status</span>
        </div>
        {[0, 1, 2].map((slot) => {
          const st = poolState(slot);
          const meta = poolCfg[slot];
          return (
            <div class={'pool ' + st.cls} key={slot}>
              <span class="purl">{meta ? `${meta.host}:${meta.port}` : meta === null ? 'not configured' : ''}</span>
              <span>{meta ? (meta.sv2 ? 'SV2' : 'SV1') : '—'}</span>
              <span>{meta?.user || '—'}</span>
              <StatusDot text={st.text} />
            </div>
          );
        })}
      </div>
    </>
  );
}
