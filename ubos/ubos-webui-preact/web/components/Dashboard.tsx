import { useEffect, useState } from 'preact/hooks';
import type { ComponentChildren } from 'preact';
import Chart from './Chart';
import { useDash } from '../lib/store';
import { getConfig } from '../lib/api';
import { fmtHr, fmtUp, fmtPower, fmtVolts } from '../lib/format';
import type { Info } from '../lib/types';

const PAUSE_LABEL: Record<string, string> = {
  dangerousTemperature: 'Stopped - Overheating',
  brokenFan: 'Stopped - Fan fault',
  poolUnreachable: 'Stopped - Pools down',
  otaUpdate: 'Stopped - Updating',
  tunerFault: 'Stopped - Tuner fault',
  userRequest: 'Stopped - User',
};

const POOL_NAMES = ['Pool 1 — Primary', 'Pool 2 — Backup', 'Pool 3 — Backup'];

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
}

function statusOf(info: Info | null): Status {
  if (!info || info.paused == null) return { text: '–', cls: '' };
  if (info.paused) {
    return {
      text: PAUSE_LABEL[info.pause_reason ?? ''] || 'Stopped',
      cls: info.pause_reason === 'userRequest' ? 'stopped' : 'warn',
    };
  }
  if (!info.running) return { text: 'Stopped', cls: 'stopped' };
  return { text: 'Running', cls: 'running' };
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

interface StatusDotProps {
  alive: boolean;
  text: string;
}

function StatusDot({ alive, text }: StatusDotProps) {
  return (
    <span class="ps">
      <svg class="dot"><use href={alive ? '#sok' : '#serr'} /></svg>
      <span class="pt">{text}</span>
    </span>
  );
}

export default function Dashboard() {
  const dash = useDash();
  const info = dash.info;
  const status = statusOf(info);

  const [poolCfg, setPoolCfg] = useState<(PoolMeta | null | undefined)[]>([undefined, undefined, undefined]);
  const [cursor, setCursor] = useState<number | null>(null);

  useEffect(() => {
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
      } catch (_) {
        /* leave labels blank; the poll still drives alive/dead */
      }
    })();
  }, []);

  function poolState(slot: number): Status {
    const mask = (info?.pools ?? 0) | 0;
    const alive = (mask >> slot) & 1;
    const meta = poolCfg[slot];
    if (alive) return { cls: 'live', text: 'Alive' };
    if (meta === null) return { cls: 'empty', text: 'Empty' };
    return { cls: '', text: 'Dead' };
  }

  return (
    <>
      <div class="left" onPointerLeave={() => setCursor(null)}>
        <Chart
          lines={[{ buf: dash.hr, color: '#6b50ff', label: 'Hashrate' }]}
          min={0}
          max={2e12}
          maj={1e12}
          minTick={5e11}
          minSpan={2e11}
          axis={(v) => {
            const t = v / 1e12;
            return (Number.isInteger(t) ? t.toFixed(0) : t.toFixed(1)) + 'T';
          }}
          val={fmtHr}
          cursor={cursor}
          onCursor={setCursor}
        />
        <Chart
          lines={[{ buf: dash.tp, color: '#fe8431', label: 'Temperature' }]}
          min={20}
          max={100}
          maj={20}
          minTick={10}
          minSpan={20}
          axis={(v) => v + '°C'}
          val={(v) => v + ' °C'}
          cursor={cursor}
          onCursor={setCursor}
        />
        <Chart
          lines={[
            { buf: dash.pin, color: '#f9c74f', label: 'Power IN' },
            { buf: dash.pout, color: '#4cc9f0', label: 'Power OUT' },
          ]}
          min={0}
          max={60}
          maj={20}
          minTick={10}
          minSpan={10}
          axis={(v) => v + 'W'}
          val={(v) => v.toFixed(0) + ' W'}
          cursor={cursor}
          onCursor={setCursor}
        />
      </div>

      <aside class="side">
        <StatCard label="Hashrate" value={fmtHr(info?.hashrate)} />
        <StatCard label="Temperature" value={info?.temperature ? info.temperature + ' °C' : '–'} />
        <StatCard label="Status" value={status.text} cls={status.cls} />
        <StatCard label="Uptime" value={fmtUp(info?.uptime)} />
        <div class={'card lkc wide' + (!dash.link ? ' dead' : '') + (dash.link ? ' live' : '')}>
          <div class="k">Link</div>
          <StatusDot alive={dash.link} text={dash.link ? 'Live' : 'Dead'} />
        </div>
        <StatCard label="Time UTC" value={info?.time || '–'} wide />
        <StatCard
          label="Power in / out"
          value={<><span>{fmtPower(info?.power_in_mw)}</span> / <span>{fmtPower(info?.power_out_mw)}</span></>}
          wide
        />
        <StatCard
          label="Voltage set / measured"
          value={<><span>{fmtVolts(info?.voltage_set_mv)}</span> / <span>{fmtVolts(info?.voltage_measured_mv)}</span></>}
          wide
        />
        <StatCard
          label="Fan speed / PWM"
          value={<><span>{info?.fan_rpm ? info.fan_rpm + ' RPM' : '–'}</span> / <span>{info?.fan_pwm ? info.fan_pwm + ' %' : '–'}</span></>}
          wide
        />
      </aside>

      <div class="card pools">
        <div class="k">Pools</div>
        {POOL_NAMES.map((name, slot) => {
          const st = poolState(slot);
          const meta = poolCfg[slot];
          return (
            <div class={'pool ' + st.cls} key={slot}>
              <div class="pm">
                <span>{name}</span>
                <span class="pi">
                  {meta
                    ? `${meta.host}:${meta.port} · ${meta.sv2 ? 'SV2' : 'SV1'}${meta.user ? ` · ${meta.user}` : ''}`
                    : meta === null
                      ? 'not configured'
                      : ''}
                </span>
              </div>
              <StatusDot alive={st.cls === 'live'} text={st.text} />
            </div>
          );
        })}
      </div>
    </>
  );
}
