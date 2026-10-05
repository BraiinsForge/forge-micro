import { useEffect, useState } from 'preact/hooks';
import type { JSX } from 'preact';
import { getJSON, postOnce } from '../lib/api';
import { ipToBytes, bytesToIp, prefixToMask, maskToPrefix } from '../lib/ip';
import { setNetChanged } from '../lib/net';
import { toast } from '../lib/toast';
import type { NetMode, NetworkConfig } from '../lib/types';
import Field from './Field';
import Checkbox from './Checkbox';
import Section from './Section';

export interface ConfigNetworkProps {
  hostname?: string;
}

interface Review {
  mode: NetMode;
  summary: string;
}

export default function ConfigNetwork({ hostname: initialHostname }: ConfigNetworkProps) {
  const [hostname, setHostname] = useState(() => initialHostname || '');
  const [appliedHostname] = useState(() => initialHostname || '');
  const [mode, setMode] = useState<'Dhcp' | 'Static'>('Dhcp');
  const [addr, setAddr] = useState('');
  const [mask, setMask] = useState('');
  const [gw, setGw] = useState('');
  const [dns1, setDns1] = useState('');
  const [dns2, setDns2] = useState('');

  const [review, setReview] = useState<Review | null>(null);
  const [confirmAfter, setConfirmAfter] = useState(true);
  const [applying, setApplying] = useState(false);

  const isStatic = mode === 'Static';

  useEffect(() => {
    (async () => {
      try {
        const data = await getJSON<NetworkConfig>('/api/system/network');
        setMode(data.mode === 'static' ? 'Static' : 'Dhcp');
        if (data.address) setAddr(bytesToIp(data.address));
        if (data.prefix_len != null) setMask(prefixToMask(data.prefix_len));
        if (data.gateway) setGw(bytesToIp(data.gateway));
        const dns = data.dns || [];
        setDns1(dns[0] ? bytesToIp(dns[0]) : '');
        setDns2(dns[1] ? bytesToIp(dns[1]) : '');
      } catch (_) {
        /* leave fields as-is on failure */
      }
    })();
  }, []);

  function openReview(event: JSX.TargetedEvent<HTMLFormElement, SubmitEvent>) {
    event.preventDefault();
    if (hostname && (!/^[a-zA-Z0-9-]+$/.test(hostname) || hostname.startsWith('-') || hostname.endsWith('-'))) {
      toast.err('Hostname may use only letters, digits, and hyphens, not at the ends.');
      return;
    }
    let modeValue: NetMode;
    let summary: string;
    if (!isStatic) {
      modeValue = 'Dhcp';
      summary = 'Switch to DHCP';
    } else {
      const address = ipToBytes(addr);
      if (!address) return toast.err('Invalid address.');
      const gateway = ipToBytes(gw);
      if (!gateway) return toast.err('Invalid gateway.');
      const prefix_len = maskToPrefix(mask);
      if (prefix_len === null) return toast.err('Invalid mask.');
      const dns: number[][] = [];
      for (const [id, value] of [
        ['DNS Primary', dns1],
        ['DNS Backup', dns2],
      ]) {
        const v = value.trim();
        if (!v) continue;
        const parsed = ipToBytes(v);
        if (!parsed) return toast.err(`Invalid ${id} address.`);
        dns.push(parsed);
      }
      modeValue = { Static: { address, prefix_len, gateway, dns } };
      summary = `Switch to static ${addr.trim()}/${prefix_len} gw ${gw.trim()}`;
    }
    setConfirmAfter(true);
    setReview({ mode: modeValue, summary });
    return;
  }

  async function apply() {
    if (!review || applying) return;
    setApplying(true);
    const { mode: modeValue } = review;
    try {
      const response = await postOnce('/api/system/network', {
        json: { mode: modeValue, hostname, confirm_after_switch: confirmAfter },
      });
      if (!response.ok) {
        const body = await response.json().catch(() => ({}));
        setApplying(false);
        setReview(null);
        return toast.err('Network change failed', body.message || 'HTTP ' + response.status);
      }
      setNetChanged({ mode: modeValue, hostname, confirmAfter, appliedHostname });
    } catch (_) {
      setApplying(false);
      setReview(null);
      toast.err('Could not reach the board to apply the change. Try again.');
    }
    return;
  }

  return (
    <>
      <h2 class="cfgTitle">Network</h2>
      <form onSubmit={openReview}>
        <div class="psec sep">
          <div class="fldRows">
            <Field
              label="Hostname"
              placeholder="Default"
              value={hostname}
              onInput={(e) => setHostname(e.currentTarget.value)}
              type="text"
              maxlength={32}
            />
          </div>
        </div>
        <Section title="Network" sep>
            <div class="fld">
              <span class="fldText">
                <span class="fldLabel">Protocol</span>
              </span>
              <span class="fldCtl">
                <div class="seg">
                  <button type="button" class={!isStatic ? 'active' : ''} onClick={() => setMode('Dhcp')}>
                    DHCP
                  </button>
                  <button type="button" class={isStatic ? 'active' : ''} onClick={() => setMode('Static')}>
                    Static
                  </button>
                </div>
              </span>
            </div>
            <Field
              label="Address"
              value={addr}
              onInput={(e) => setAddr(e.currentTarget.value)}
              type="text"
              placeholder="192.168.1.50"
              disabled={!isStatic}
              off={!isStatic}
            />
            <Field
              label="Mask"
              value={mask}
              onInput={(e) => setMask(e.currentTarget.value)}
              type="text"
              placeholder="255.255.255.0"
              disabled={!isStatic}
              off={!isStatic}
            />
            <Field
              label="Gateway"
              value={gw}
              onInput={(e) => setGw(e.currentTarget.value)}
              type="text"
              placeholder="192.168.1.1"
              disabled={!isStatic}
              off={!isStatic}
            />
            <Field
              label="DNS Primary"
              value={dns1}
              onInput={(e) => setDns1(e.currentTarget.value)}
              type="text"
              placeholder="8.8.8.8"
              disabled={!isStatic}
              off={!isStatic}
            />
            <Field
              label="DNS Backup"
              value={dns2}
              onInput={(e) => setDns2(e.currentTarget.value)}
              type="text"
              placeholder="1.1.1.1"
              disabled={!isStatic}
              off={!isStatic}
            />
        </Section>
        <button type="submit">Apply</button>
      </form>

      {review && (
        <div class="modal">
          <div>
            <p>{review.summary}</p>
            <Checkbox
              class="modalCheck"
              label="Confirm after switch"
              checked={confirmAfter}
              onInput={(e) => setConfirmAfter(e.currentTarget.checked)}
              disabled={isStatic}
            />
            {!confirmAfter && (
              <p class="modalWarn">Warning: unreachable config will not auto-revert; serial recovery needed.</p>
            )}
            <div class="modalBtns">
              <button type="button" onClick={apply} disabled={applying}>
                Apply
              </button>
              <button type="button" class="btnSecondary" onClick={() => setReview(null)}>
                Cancel
              </button>
            </div>
          </div>
        </div>
      )}
    </>
  );
}
