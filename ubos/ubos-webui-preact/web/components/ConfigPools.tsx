import { useState } from 'preact/hooks';
import type { JSX } from 'preact';
import { postConfig } from '../lib/api';
import { toast } from '../lib/toast';
import type { PoolConfig, PoolProtocol } from '../lib/types';
import Field from './Field';
import Select from './Select';
import Section from './Section';

const ADDR_RE = /^([^:/]+)(?::(\d+))?$/;
const URL_RE = /^(?:(stratum2?\+tcp):\/\/)?(.*)$/;
const NAMES = ['Pool 1 — Primary', 'Pool 2 — Backup', 'Pool 3 — Backup'];
const PROTOS = [
  { value: 'stratum+tcp', label: 'stratum+tcp' },
  { value: 'stratum2+tcp', label: 'stratum2+tcp' },
];

const bytesToHex = (bytes: number[]): string =>
  Array.from(bytes, (v) => v.toString(16).padStart(2, '0')).join('');

function hexToBytes(hex: string): number[] {
  const bytes = [];
  for (let i = 0; i < hex.length; i += 2) bytes.push(parseInt(hex.slice(i, i + 2), 16));
  return bytes;
}

interface Slot {
  proto: string;
  addr: string;
  user: string;
  pass: string;
  key: string;
}

function initSlot(pool: PoolConfig | null | undefined): Slot {
  if (!pool) return { proto: 'stratum+tcp', addr: '', user: '', pass: '', key: '' };
  const protocol = pool.protocol;
  const sv2 = protocol && 'Sv2' in protocol ? protocol.Sv2 : undefined;
  const sv1 = protocol && 'Sv1' in protocol ? protocol.Sv1 : undefined;
  return {
    proto: sv2 ? 'stratum2+tcp' : 'stratum+tcp',
    addr: pool.host + (pool.port ? ':' + pool.port : ''),
    user: pool.user || '',
    pass: sv1?.password || '',
    key: sv2?.pool_pubkey ? bytesToHex(sv2.pool_pubkey) : '',
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
}

export default function ConfigPools({ pools }: ConfigPoolsProps) {
  const [slots, setSlots] = useState(() => [0, 1, 2].map((i) => initSlot(pools[i])));

  function patch(i: number, field: keyof Slot, value: string): void {
    setSlots((prev) => prev.map((slot, idx) => (idx === i ? { ...slot, [field]: value } : slot)));
  }

  function onPaste(i: number, e: JSX.TargetedEvent<HTMLInputElement, ClipboardEvent>): void {
    const text = e.clipboardData?.getData('text').trim() ?? '';
    const match = text.match(URL_RE);
    if (match && match[1]) {
      e.preventDefault();
      const proto = match[1];
      const slash = match[2].indexOf('/');
      const addr = slash < 0 ? match[2] : match[2].slice(0, slash);
      const key = slash < 0 ? '' : match[2].slice(slash + 1);
      setSlots((prev) =>
        prev.map((slot, idx) =>
          idx === i ? { ...slot, proto, addr, key: key || slot.key } : slot,
        ),
      );
    }
  }

  function onCopy(i: number, e: JSX.TargetedEvent<HTMLInputElement, ClipboardEvent>): void {
    e.preventDefault();
    const slot = slots[i];
    const key = slot.proto === 'stratum2+tcp' && slot.key.trim() ? '/' + slot.key.trim() : '';
    e.clipboardData?.setData('text/plain', slot.proto + '://' + slot.addr + key);
  }

  async function save(event: JSX.TargetedEvent<HTMLFormElement, SubmitEvent>): Promise<void> {
    event.preventDefault();
    const pool: PoolPatch[] = [];
    for (let i = 0; i < 3; i++) {
      const slot = slots[i];
      const addr = slot.addr.trim();
      if (!addr) continue;
      const match = addr.match(ADDR_RE);
      if (!match) {
        toast.err(`Invalid pool ${i + 1} address`);
        return;
      }
      const host = match[1];
      const port = match[2] ? +match[2] : 3333;
      const user = slot.user || '';
      let protocol: PoolProtocol;
      if (slot.proto === 'stratum2+tcp') {
        let pool_pubkey: number[] | null = null;
        const hex = slot.key.trim();
        if (hex) {
          if (!/^[0-9a-fA-F]{64}$/.test(hex)) {
            toast.err(`Pool ${i + 1}: pubkey must be 64 hex chars`);
            return;
          }
          pool_pubkey = hexToBytes(hex);
        }
        protocol = { Sv2: { pool_pubkey } };
      } else {
        protocol = { Sv1: { password: slot.pass || null } };
      }
      pool.push({ host, port, user, protocol });
    }
    try {
      await postConfig({ pool });
      toast.ok('Changes saved');
    } catch (error) {
      toast.err('Save failed', error instanceof Error ? error.message : String(error));
    }
  }

  return (
    <form onSubmit={save}>
      {NAMES.map((name, i) => (
        <Section title={name} key={i}>
          <div class="pcfg" data-protocol={slots[i].proto === 'stratum2+tcp' ? 'sv2' : 'sv1'}>
            <Field
              label="URL"
              class="urlRow"
              value={slots[i].addr}
              onInput={(e) => patch(i, 'addr', e.currentTarget.value)}
              onPaste={(e) => onPaste(i, e)}
              onCopy={(e) => onCopy(i, e)}
              placeholder="host:port"
              labelEnd={
                <span class="urlProto">
                  <Select
                    value={slots[i].proto}
                    onInput={(e) => patch(i, 'proto', e.currentTarget.value)}
                    options={PROTOS}
                  />
                  <span class="urlSep">://</span>
                </span>
              }
            />
            <Field
              label="Username"
              value={slots[i].user}
              onInput={(e) => patch(i, 'user', e.currentTarget.value)}
            />
            <Field
              class="s1"
              label="Password"
              value={slots[i].pass}
              onInput={(e) => patch(i, 'pass', e.currentTarget.value)}
              type="password"
            />
            <Field
              class="s2 wide"
              label="Authority pubkey (optional)"
              desc="64 hex characters. If left empty, trust pool (skip authority check)."
              value={slots[i].key}
              onInput={(e) => patch(i, 'key', e.currentTarget.value)}
              placeholder="64 hex chars"
            />
          </div>
        </Section>
      ))}
      <button type="submit">Save</button>
    </form>
  );
}
