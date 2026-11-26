import { useState } from 'preact/hooks';
import type { TargetedEvent } from 'preact';
import { saveConfig } from '../lib/api';
import { toast } from '../lib/toast';
import type { PoolConfig, PoolProtocol } from '../lib/types';

const ADDR_RE = /^([^:/]+)(?::(\d+))?$/;
const URL_RE = /^(?:(stratum2?\+tcp):\/\/)?([^/]*)(?:\/(.*))?$/;
const DEFAULT_PORT = 3333;

// The authority key travels as base58check, so the alphabet excludes
// the four ambiguous glyphs 0, O, I and l. A 32-byte payload plus its
// version and checksum bytes lands in this length range. The board owns
// checksum validation; this only rejects input that cannot be a key.
const PUBKEY_RE = /^[1-9A-HJ-NP-Za-km-z]{40,60}$/;

interface Slot {
  url: string;
  user: string;
  pass: string;
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
  if (!pool) return { url: '', user: '', pass: '' };
  const sv2 = pool.protocol && 'Sv2' in pool.protocol ? pool.protocol.Sv2 : undefined;
  const sv1 = pool.protocol && 'Sv1' in pool.protocol ? pool.protocol.Sv1 : undefined;
  const proto = sv2 ? 'stratum2+tcp' : 'stratum+tcp';
  const key = sv2?.pool_pubkey ? '/' + sv2.pool_pubkey : '';
  const addr = pool.host + (pool.port ? ':' + pool.port : '');
  return {
    url: proto + '://' + addr + key,
    user: pool.user || '',
    pass: sv1?.password || '',
  };
}

export interface ConfigPoolsProps {
  pools: (PoolConfig | null)[];
  /** The board's fee pool, shown read-only after the operator's rows. */
  fee?: PoolConfig | null;
  /** The share the fee slot takes, in thousandths, shown on its row. */
  feePermille?: number;
}

interface PoolPatch {
  host: string;
  port: number;
  user: string;
  protocol: PoolProtocol;
}

export default function ConfigPools({ pools, fee, feePermille }: ConfigPoolsProps) {
  const [slots, setSlots] = useState(() => [0, 1, 2].map((i) => initSlot(pools[i])));

  function patch(i: number, field: keyof Slot, value: string): void {
    setSlots((prev) => prev.map((slot, idx) => (idx === i ? { ...slot, [field]: value } : slot)));
  }

  async function save(event: TargetedEvent<HTMLFormElement, SubmitEvent>): Promise<void> {
    event.preventDefault();
    // Row order is the priority: the board mines the first pool and fails over down the list.
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
      let protocol: PoolProtocol;
      if (parsed.sv2) {
        let pool_pubkey: string | null = null;
        if (parsed.key) {
          if (!PUBKEY_RE.test(parsed.key)) {
            toast.err(`Pool ${i + 1}: the pubkey in the URL is not valid base58`);
            return;
          }
          pool_pubkey = parsed.key;
        }
        protocol = { Sv2: { pool_pubkey } };
      } else {
        protocol = { Sv1: { password: slot.pass || null } };
      }
      pool.push({ host, port, user, protocol });
    }
    await saveConfig({ pool });
  }

  return (
    <>
      <h2 class="cfgTitle">Pools</h2>
      <form onSubmit={save}>
        <div class="poolTable">
          <div class="poolHead">
            <span>Pool URL</span>
            <span>Username</span>
            <span>Password</span>
          </div>
          {slots.map((slot, i) => {
            const parsed = parseUrl(slot.url);
            return (
              <div class="poolRow" key={i}>
                <input
                  class="poolInp"
                  value={slot.url}
                  onInput={(e) => patch(i, 'url', e.currentTarget.value)}
                  placeholder="stratum2+tcp://host:port/pubkey"
                />
                <input
                  class="poolInp"
                  value={slot.user}
                  onInput={(e) => patch(i, 'user', e.currentTarget.value)}
                  placeholder="Username"
                />
                {parsed.sv2 && !parsed.key ? (
                  <span class="poolTrust">
                    Trust pool
                    <br />
                    (skip authority check)
                  </span>
                ) : (
                  // Sv2 carries no password, so its box stays empty and offers no hint.
                  <input
                    class="poolInp"
                    type="password"
                    value={parsed.sv2 ? '' : slot.pass}
                    onInput={(e) => patch(i, 'pass', e.currentTarget.value)}
                    placeholder={parsed.sv2 ? '' : 'Password'}
                    disabled={parsed.sv2}
                  />
                )}
              </div>
            );
          })}
          {fee && (
            <div class="poolRow">
              <input class="poolInp" value={initSlot(fee).url} disabled />
              <input class="poolInp" value={fee.user} disabled />
              <span class="poolTrust">
                Dev fee{feePermille != null && ` ${feePermille / 10} %`}
                <br />
                (fixed share, not editable)
              </span>
            </div>
          )}
        </div>
        <button type="submit">Save</button>
      </form>
    </>
  );
}
