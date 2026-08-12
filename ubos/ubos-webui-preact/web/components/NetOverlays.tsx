import { useState } from 'preact/hooks';
import { useDash } from '../lib/store';
import { useNet } from '../lib/net';
import { postJSON } from '../lib/api';
import { bytesToIp } from '../lib/ip';

export default function NetOverlays() {
  const dash = useDash();
  const net = useNet();

  // Seconds remaining on a pending confirm-or-revert window, from the
  // poll. Non-zero means a network change awaits confirmation — shown on
  // a freshly reconnected session, so the operator can confirm/revert
  // from whatever view they land on. `| 0` also collapses a missing
  // value (undefined) to 0.
  const pending = (dash.info?.net_confirm_seconds ?? 0) | 0;

  const [msg, setMsg] = useState('');
  const [busy, setBusy] = useState(false);

  // Static switch exposes the new address as a link; DHCP does not.
  // `mode` is 'Dhcp' (a bare string) or a { Static: … } object, so a
  // typeof check narrows out the string case before reading `.Static`.
  let switchedUrl: string | null = null;
  const change = net.changed;
  if (change && typeof change.mode !== 'string' && change.mode.Static) {
    switchedUrl = `${location.protocol}//${bytesToIp(change.mode.Static.address)}/`;
  }

  const renamed =
    net.changed && net.changed.hostname && net.changed.hostname !== net.changed.appliedHostname
      ? net.changed.hostname
      : null;

  async function post(path: string, label: string) {
    if (busy) return;
    setBusy(true);
    setMsg('');
    try {
      await postJSON(path, {});
      // The next poll reports net_confirm_seconds = 0 and this overlay
      // hides itself.
    } catch (error) {
      // `error` is unknown at the catch boundary; narrow to Error to
      // read its message, falling back for a non-Error throw.
      const message = error instanceof Error ? error.message : 'unknown';
      setMsg(`${label} failed: ${message}`);
    } finally {
      setBusy(false);
    }
  }

  if (net.changed) {
    return (
      <div class="fullNotice">
        <div>
          <h1>Network configuration changed</h1>
          <p>
            {switchedUrl ? (
              <>
                The board is now at <a href={switchedUrl}>{switchedUrl}</a>.
              </>
            ) : (
              'The board is switching to a new DHCP address.'
            )}{' '}
            It can take up to 30 seconds to become reachable, then log in again.{' '}
            {net.changed.confirmAfter && 'Log in and confirm within 2 minutes, or the change reverts.'}{' '}
            {renamed && `The new name ${renamed}.local takes effect after a reboot.`}
          </p>
        </div>
      </div>
    );
  }

  if (pending > 0) {
    return (
      <div class="modal">
        <div>
          <p>Network change pending. Confirm within {pending}s, or it reverts automatically.</p>
          <div class="modalBtns">
            <button type="button" onClick={() => post('/api/system/network/confirm', 'Confirm')} disabled={busy}>
              Confirm
            </button>
            <button
              type="button"
              class="btnDanger"
              onClick={() => post('/api/system/network/revert', 'Revert')}
              disabled={busy}
            >
              Revert
            </button>
          </div>
          {msg && (
            <div class="msg" style={{ color: 'var(--red)' }}>
              {msg}
            </div>
          )}
        </div>
      </div>
    );
  }

  return null;
}
