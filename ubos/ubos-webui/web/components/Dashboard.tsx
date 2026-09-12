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

function StatusDot({ text }: { text: string }) {
  return (
    <span class="ps">
      <span class="pd" />
      <span class="pt">{text}</span>
    </span>
  );
}

export default function Dashboard() {
  const dash = useDash();
  const info = dash.info;
  const status = statusOf(info);
  const measuredVoltage = info?.psu_voltage_measured_mv || info?.chain_voltage_mv;

  const [poolCfg, setPoolCfg] = useState<(PoolMeta | null | undefined)[]>([undefined, undefined, undefined]);
  const [tempThresh, setTempThresh] = useState<[number, number] | null>(null);
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
        if (data.thermal) {
          setTempThresh([data.thermal.dangerous_temp_c, data.thermal.dangerous_chip_temp_c]);
        }
      } catch (_) {
        /* leave labels blank; the poll still drives alive/dead */
      }
    })();
  }, []);

  function tempCls(): string {
    if (!info?.temperature || !tempThresh) return '';
    if (info.temperature >= tempThresh[1]) return 'temp-red';
    if (info.temperature >= tempThresh[0]) return 'temp-warn';
    return 'temp-ok';
  }

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
          lines={[{ buf: dash.tp, color: '#fe8431', label: 'Temperature (°C)' }]}
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
        <StatCard label="Chip Temperature" value={info?.temperature ? info.temperature + ' °C' : '–'} cls={tempCls()} />
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
          <StatusDot text={status.text} />
        </div>
        <div class={'card lkc' + (dash.link ? ' live' : ' dead')}>
          <div class="k">Link</div>
          <StatusDot text={dash.link ? 'Live' : 'Dead'} />
        </div>
        <StatCard label="Time UTC" value={info?.time || '–'} wide />
      </aside>

      <div class="card pools">
        <div class="ph">
          <div class="k">Pools</div>
          <a class="plink" href="#config/pools">Configure</a>
        </div>
        <div class="ptr">
          <span>URL</span>
          <span>Protocol</span>
          <span>User</span>
          <span>Status</span>
        </div>
        {[0, 1, 2].map((slot) => {
          const st = poolState(slot);
          const meta = poolCfg[slot];
          return (
            <div class={'pool ' + st.cls} key={slot}>
              <span>{meta ? `${meta.host}:${meta.port}` : meta === null ? 'not configured' : ''}</span>
              <span>{meta ? (meta.sv2 ? 'SV2' : 'SV1') : '—'}</span>
              <span>{meta?.user || '—'}</span>
              <span class="ps"><span class="pd" />{st.text}</span>
            </div>
          );
        })}
      </div>
    </>
  );
}
