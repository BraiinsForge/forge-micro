import { useState } from 'preact/hooks';
import type { JSX } from 'preact';
import { API, loginRedirect, postConfig, postOnce } from '../lib/api';
import { useDash, startPoll, stopPoll } from '../lib/store';
import { toast } from '../lib/toast';
import type { ApiCredentials } from '../lib/types';
import type { BoardNotice } from './Config';
import Field from './Field';
import Section from './Section';

const OTA_MAX_DEFAULT = 952 * 1024;

export interface ConfigSystemProps {
  api?: ApiCredentials;
  boardNotice: BoardNotice;
}

export default function ConfigSystem({ api, boardNotice }: ConfigSystemProps) {
  const [sysUser, setSysUser] = useState(() => (api && api.username) || '');
  const [sysPass, setSysPass] = useState('');

  async function saveCreds(event: JSX.TargetedEvent<HTMLFormElement, SubmitEvent>): Promise<void> {
    event.preventDefault();
    try {
      await postConfig({ api: { username: sysUser, password: sysPass } });
      toast.ok('Changes saved');
    } catch (error) {
      toast.err('Save failed', error instanceof Error ? error.message : String(error));
    }
  }

  const dash = useDash();
  const ota = dash.info?.ota;
  const [fwFile, setFwFile] = useState<FileList | null>(null);
  const [uploading, setUploading] = useState(false);
  const [progress, setProgress] = useState(0);

  const blockUnload = (event: Event) => event.preventDefault();

  function otaFail(text: string): void {
    removeEventListener('beforeunload', blockUnload);
    setUploading(false);
    startPoll();
    toast.err('Firmware update failed', text);
  }

  async function startOta() {
    const file = fwFile && fwFile[0];
    if (!file) return toast.err('Select a firmware image first.');
    const max = (ota && ota.maxImageBytes) || OTA_MAX_DEFAULT;
    if (file.size > max) {
      return toast.err(
        'File too large',
        `${(file.size / 1024).toFixed(0)} KiB exceeds the ${max / 1024} KiB maximum.`
      );
    }
    stopPoll();
    setUploading(true);
    setProgress(0);
    addEventListener('beforeunload', blockUnload);

    let bytes: Uint8Array;
    try {
      bytes = new Uint8Array(await file.arrayBuffer());
    } catch (error) {
      return otaFail('Could not read file. ' + (error instanceof Error ? error.message : 'Unknown error.'));
    }
    const total = bytes.length;
    const CHUNK = 4096;
    const MAX_RESTARTS = 2;
    let sessionRestarts = 0;
    let miningRestarts = 0;
    let offset = 0;

    while (offset < total) {
      const end = Math.min(offset + CHUNK, total);
      const sector = bytes.subarray(offset, end);
      const isFinal = end >= total;
      const headers: Record<string, string> = { 'X-OTA-Offset': String(offset) };
      if (isFinal) headers['X-OTA-Final'] = '1';

      let response: Response | null = null;
      const MAX_RETRIES = 3;
      for (let attempt = 0; attempt <= MAX_RETRIES; attempt++) {
        if (attempt > 0) await new Promise((r) => setTimeout(r, 500));
        const controller = isFinal ? new AbortController() : null;
        const timer = controller ? setTimeout(() => controller.abort(), 3000) : null;
        try {
          // `subarray()` types its result as `Uint8Array<ArrayBufferLike>`,
          // which no longer structurally matches `BodyInit`'s
          // ArrayBuffer-specific `BufferSource` under TS 5.7's typed-array
          // generics; the bytes underneath are a plain ArrayBuffer.
          if (offset === 0) {
            response = await postOnce('/api/system/OTA/chunk', {
              body: sector as BufferSource,
              headers,
              signal: controller?.signal,
            });
          } else {
            response = await fetch(API + '/api/system/OTA/chunk', {
              method: 'POST',
              credentials: 'include',
              headers,
              body: sector as BufferSource,
              signal: controller?.signal,
            });
          }
          if (timer) clearTimeout(timer);
          break;
        } catch (_) {
          if (timer) clearTimeout(timer);
          if (isFinal) {
            removeEventListener('beforeunload', blockUnload);
            return boardNotice(
              'Updating firmware',
              'Firmware uploaded. The board is rebooting into the new image.',
              true
            );
          }
          if (attempt === MAX_RETRIES) {
            return otaFail(
              'Upload failed because of a network error. The board will resume mining shortly.'
            );
          }
        }
      }

      // `response` is always set by this point: the loop above only falls
      // through without it when the final attempt already returned above.
      // The guard exists to satisfy strict null checks on the reads below.
      if (!response) {
        return otaFail('Upload failed because of a network error. The board will resume mining shortly.');
      }

      // A 401 mid-upload means the session died; bounce to login.
      if (response && response.status === 401) return loginRedirect();

      if (isFinal) {
        if (response && response.ok) {
          removeEventListener('beforeunload', blockUnload);
          return boardNotice(
            'Updating firmware',
            'Firmware uploaded. The board is rebooting into the new image.',
            true
          );
        }
        let detail = 'HTTP ' + (response ? response.status : 'unknown');
        try {
          detail = (await response.json())?.message || detail;
        } catch (_) {}
        return otaFail(detail);
      }

      if (response.status === 409) {
        if (++sessionRestarts > MAX_RESTARTS) return otaFail('Upload failed because the session reset repeatedly.');
        offset = 0;
        continue;
      }
      if (response.status === 503 && offset === 0) {
        if (++miningRestarts > MAX_RESTARTS) {
          let detail = 'Upload failed because the board could not stop mining.';
          try {
            detail = (await response.json())?.message || detail;
          } catch (_) {}
          return otaFail(detail);
        }
        await new Promise((r) => setTimeout(r, 1000));
        continue;
      }
      if (!response.ok) {
        let detail = 'HTTP ' + response.status;
        try {
          detail = (await response.json())?.message || detail;
        } catch (_) {}
        return otaFail(detail);
      }

      let ack: { next?: number } = {};
      try {
        ack = await response.json();
      } catch (_) {}
      offset = typeof ack.next === 'number' ? ack.next : end;
      setProgress(Math.round((offset / total) * 100));
    }
  }

  const [resetConfirm, setResetConfirm] = useState(false);

  async function factoryReset(): Promise<void> {
    stopPoll();
    try {
      const response = await postOnce('/api/system/factory_reset', { json: {} });
      if (!response.ok) {
        startPoll();
        toast.err('Reset failed', 'HTTP ' + response.status);
        return;
      }
    } catch (_) {
      startPoll();
      toast.err('Could not reach the board to reset. Try again.');
      return;
    }
    setResetConfirm(false);
    boardNotice(
      'Factory reset complete',
      'The board has been reset and is rebooting to DHCP with default credentials.' +
        ' It can take up to 30 seconds to become reachable, then log in again.',
      false
    );
  }

  return (
    <>
      <form onSubmit={saveCreds}>
        <Section title="System">
            <Field
              label="Username"
              value={sysUser}
              onInput={(e) => setSysUser(e.currentTarget.value)}
              type="text"
              maxlength={32}
            />
            <Field
              label="Password"
              value={sysPass}
              onInput={(e) => setSysPass(e.currentTarget.value)}
              type="password"
              maxlength={32}
              placeholder="leave blank to keep current"
            />
        </Section>
        <button type="submit">Save</button>
      </form>

      {ota && (
        <div class="psec sep">
          <h3>Firmware update</h3>
          <div class="panel">
            <div>
              <span class="fwLabel">Running slot:</span>
              <span>{ota.runningSlot || '?'}</span>
              <span class="fwLabel fwLabelGap">Version:</span>
              <span>{ota.runningVersion != null ? ota.runningVersion : '?'}</span>
            </div>
            <div class="fwRow">
              <label for="fwFile">Firmware image</label>
              <input
                id="fwFile"
                type="file"
                accept=".bin,.aup"
                disabled={uploading}
                onChange={(e) => setFwFile(e.currentTarget.files)}
              />
              <button type="button" onClick={startOta} disabled={uploading}>
                Update…
              </button>
            </div>
            {uploading && (
              <div id="fwProgWrap">
                <div
                  id="fwProgBar"
                  role="progressbar"
                  aria-valuemin={0}
                  aria-valuemax={100}
                  aria-valuenow={progress}
                  style={{ width: progress + '%' }}
                ></div>
              </div>
            )}
          </div>
        </div>
      )}

      <div class="psec sep">
        <h3>Factory reset</h3>
        {!resetConfirm ? (
          <button type="button" class="btnDanger" onClick={() => setResetConfirm(true)}>
            Reset to Factory Defaults
          </button>
        ) : (
          <>
            <p class="modalWarn">All settings erased; board reboots to DHCP with default credentials.</p>
            <div class="modalBtns">
              <button type="button" class="btnDanger" onClick={factoryReset}>
                Confirm reset
              </button>
              <button type="button" class="btnSecondary" onClick={() => setResetConfirm(false)}>
                Cancel
              </button>
            </div>
          </>
        )}
      </div>
    </>
  );
}
