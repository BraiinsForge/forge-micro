import { useState } from 'preact/hooks';
import type { JSX } from 'preact';
import { saveConfig } from '../lib/api';
import { toast } from '../lib/toast';
import type { PoolConfig, PoolProtocol } from '../lib/types';

const ADDR_RE = /^([^:/]+)(?::(\d+))?$/;
const URL_RE = /^(?:(stratum2?\+tcp):\/\/)?([^/]*)(?:\/(.*))?$/;
const DEFAULT_PORT = 3333;
const PUBKEY_LEN = 32;

const bytesToB64 = (bytes: number[]): string => btoa(String.fromCharCode(...bytes));

function b64ToBytes(b64: string): number[] | null {
  try {
    const bytes = Array.from(atob(b64), (c) => c.charCodeAt(0));
    return bytes.length === PUBKEY_LEN ? bytes : null;
  } catch {
    return null;
  }
}

interface Slot {
  url: string;
  user: string;
  pass: string;
  on: boolean;
}

interface Parsed {
  sv2: boolean;
  hostport: string;
  key: string;
}

function parseUrl(url: string): Parsed {
  const match = url.trim().match(URL_RE);
  return {
    sv2: match?.[1] === 'stratum2+tcp',
    hostport: match?.[2] ?? '',
    key: match?.[3] ?? '',
  };
}

function initSlot(pool: PoolConfig | null | undefined): Slot {
  if (!pool) return { url: '', user: '', pass: '', on: false };
  const sv2 = pool.protocol && 'Sv2' in pool.protocol ? pool.protocol.Sv2 : undefined;
  const sv1 = pool.protocol && 'Sv1' in pool.protocol ? pool.protocol.Sv1 : undefined;
  const proto = sv2 ? 'stratum2+tcp' : 'stratum+tcp';
  const key = sv2?.pool_pubkey ? '/' + bytesToB64(sv2.pool_pubkey) : '';
  const addr = pool.host + (pool.port ? ':' + pool.port : '');
  return {
    url: proto + '://' + addr + key,
    user: pool.user || '',
    pass: sv1?.password || '',
    on: pool.weight === undefined ? !!pool.host : pool.weight > 0,
  };
}

export interface ConfigPoolsProps {
  pools: (PoolConfig | null)[];
}

interface PoolPatch {
  host: string;
  port: number;
  user: string;
  protocol: PoolProtocol;
  weight: number;
}

export default function ConfigPools({ pools }: ConfigPoolsProps) {
  const [slots, setSlots] = useState(() => [0, 1, 2].map((i) => initSlot(pools[i])));

  function patch(i: number, field: keyof Slot, value: string | boolean): void {
    setSlots((prev) => prev.map((slot, idx) => (idx === i ? { ...slot, [field]: value } : slot)));
  }

  async function save(event: JSX.TargetedEvent<HTMLFormElement, SubmitEvent>): Promise<void> {
    event.preventDefault();
    const pool: PoolPatch[] = [];
    for (let i = 0; i < 3; i++) {
      const slot = slots[i];
      const parsed = parseUrl(slot.url);
      if (!parsed.hostport) continue;
      const match = parsed.hostport.match(ADDR_RE);
      if (!match) {
        toast.err(`Invalid pool ${i + 1} address`);
        return;
      }
      const host = match[1];
      const port = match[2] ? +match[2] : DEFAULT_PORT;
      const user = slot.user || '';
      const weight = slot.on ? 1 : 0;
      let protocol: PoolProtocol;
      if (parsed.sv2) {
        let pool_pubkey: number[] | null = null;
        if (parsed.key) {
          pool_pubkey = b64ToBytes(parsed.key);
          if (!pool_pubkey) {
            toast.err(`Pool ${i + 1}: authority key must be 32 bytes of base64`);
            return;
          }
        }
        protocol = { Sv2: { pool_pubkey } };
      } else {
        protocol = { Sv1: { password: slot.pass || null } };
      }
      pool.push({ host, port, user, protocol, weight });
    }
    await saveConfig({ pool });
  }

  return (
    <>
      <h2 class="cfgTitle">Pools</h2>
      <form onSubmit={save}>
        <div class="poolTable">
          <div class="poolHead">
            <span></span>
            <span>Pool URL</span>
            <span>Username</span>
            <span>Password</span>
          </div>
          {slots.map((slot, i) => {
            const parsed = parseUrl(slot.url);
            return (
              <div class="poolRow" key={i}>
                <label class="poolSw">
                  <input
                    type="checkbox"
                    checked={slot.on}
                    onInput={(e) => patch(i, 'on', e.currentTarget.checked)}
                  />
                  <span class="poolSwTrack"></span>
                </label>
                <input
                  class="poolInp"
                  value={slot.url}
                  onInput={(e) => patch(i, 'url', e.currentTarget.value)}
                  placeholder="stratum+tcp://host:port"
                  disabled={!slot.on}
                />
                <input
                  class="poolInp"
                  value={slot.user}
                  onInput={(e) => patch(i, 'user', e.currentTarget.value)}
                  placeholder="Username"
                  disabled={!slot.on}
                />
                {parsed.sv2 ? (
                  parsed.key ? (
                    <span class="poolKey" title={parsed.key}>
                      {parsed.key}
                    </span>
                  ) : (
                    <span class="poolTrust">
                      Trust pool
                      <br />
                      (skip authority check)
                    </span>
                  )
                ) : (
                  <input
                    class="poolInp"
                    type="password"
                    value={slot.pass}
                    onInput={(e) => patch(i, 'pass', e.currentTarget.value)}
                    placeholder="Password"
                    disabled={!slot.on}
                  />
                )}
              </div>
            );
          })}
        </div>
        <button type="submit">Save</button>
      </form>
    </>
  );
}
