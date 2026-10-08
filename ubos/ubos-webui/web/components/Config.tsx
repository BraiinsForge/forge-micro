import { useEffect, useState } from 'preact/hooks';
import type { JSX } from 'preact';
import { getConfig, getJSON, RETRY_DELAYS, sleep } from '../lib/api';
import type { AsicBounds, AsicDefaults, CoolingBounds, Config as ConfigData, ThermalBounds } from '../lib/types';
import type { BoardNotice } from '../app';
import ConfigPools from './ConfigPools';
import ConfigMining from './ConfigMining';
import ConfigThermal from './ConfigThermal';
import ConfigSystem from './ConfigSystem';
import ConfigNetwork from './ConfigNetwork';

interface TabRenderProps {
  config: ConfigData | null;
  asicBounds: Partial<AsicBounds>;
  asicDefaults: AsicDefaults;
  thermalBounds: Partial<ThermalBounds>;
  coolingBounds: Partial<CoolingBounds>;
  boardNotice: BoardNotice;
}

interface Tab {
  key: string;
  label: string;
  render: (p: TabRenderProps) => JSX.Element;
}

const TABS: Tab[] = [
  { key: 'pools', label: 'Pools', render: (p) => <ConfigPools pools={p.config?.pool || []} /> },
  {
    key: 'temperature',
    label: 'Temperature',
    render: (p) => (
      <ConfigThermal
        thermal={p.config?.thermal}
        cooling={p.config?.cooling}
        thermalBounds={p.thermalBounds}
        coolingBounds={p.coolingBounds}
      />
    ),
  },
  {
    key: 'mining',
    label: 'Mining',
    render: (p) => (
      <ConfigMining
        miner={p.config?.miner}
        bounds={p.asicBounds}
        defaults={p.asicDefaults}
      />
    ),
  },
  {
    key: 'system',
    label: 'System',
    render: (p) => <ConfigSystem api={p.config?.api} boardNotice={p.boardNotice} />,
  },
  {
    key: 'network',
    label: 'Network',
    render: (p) => <ConfigNetwork hostname={p.config?.network?.hostname} />,
  },
];

function subOf(hash: string): string {
  const found = TABS.find((t) => hash === '#config/' + t.key);
  return found ? found.key : 'pools';
}

export interface ConfigProps {
  hash: string;
  boardNotice: BoardNotice;
}

export default function Config({ hash, boardNotice }: ConfigProps) {
  const sub = subOf(hash);

  const [config, setConfig] = useState<ConfigData | null>(null);
  const [asicBounds, setAsicBounds] = useState<Partial<AsicBounds>>({});
  const [asicDefaults, setAsicDefaults] = useState<AsicDefaults>({});
  const [thermalBounds, setThermalBounds] = useState<Partial<ThermalBounds>>({});
  const [coolingBounds, setCoolingBounds] = useState<Partial<CoolingBounds>>({});
  const [loadError, setLoadError] = useState('');

  useEffect(() => {
    (async () => {
      let lastError: unknown;
      for (const wait of RETRY_DELAYS) {
        if (wait) await sleep(wait);
        try {
          const [cfg, asic, thermal, cooling] = await Promise.all([
            getConfig(),
            getJSON<{ bounds: AsicBounds } & AsicDefaults>('/api/system/asic'),
            getJSON<{ bounds: ThermalBounds }>('/api/system/thermal'),
            getJSON<{ bounds: CoolingBounds }>('/api/system/cooling'),
          ]);
          setConfig(cfg);
          setAsicBounds(asic.bounds || {});
          setAsicDefaults(asic);
          setThermalBounds(thermal.bounds || {});
          setCoolingBounds(cooling.bounds || {});
          setLoadError('');
          return;
        } catch (error) {
          lastError = error;
        }
      }
      setLoadError('Load failed: ' + (lastError instanceof Error ? lastError.message : 'unknown'));
    })();
  }, []);

  const props: TabRenderProps = {
    config,
    asicBounds,
    asicDefaults,
    thermalBounds,
    coolingBounds,
    boardNotice,
  };

  return (
    <section class="cfg">
      <nav class="cfgnav">
        {TABS.map((t) => (
          <a key={t.key} href={'#config/' + t.key} class={sub === t.key ? 'on' : ''}>
            {t.label}
          </a>
        ))}
      </nav>
      <div class="cfgpanel">
        {loadError ? (
          <div class="msg" style={{ color: 'var(--red)' }}>{loadError}</div>
        ) : !config ? (
          <div class="msg">Loading…</div>
        ) : (
          TABS.map((t) => (
            <div key={t.key} class={'cfgsec' + (sub === t.key ? ' on' : '')}>
              {t.render(props)}
            </div>
          ))
        )}
      </div>
    </section>
  );
}
