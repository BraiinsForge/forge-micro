import { useEffect, useRef, useState } from 'preact/hooks';
import { postJSON } from '../lib/api';
import type { ApiError } from '../lib/api';
import { useDash } from '../lib/store';
import type { BoardNotice } from '../app';

export interface ActionMenuProps {
  boardNotice: BoardNotice;
}

export default function ActionMenu({ boardNotice }: ActionMenuProps) {
  const dash = useDash();

  const [open, setOpen] = useState(false);
  // Which action is waiting on its confirm step, if any.
  const [confirming, setConfirming] = useState<'restart' | 'thermal' | null>(null);
  const [inFlight, setInFlight] = useState(false);
  const [msg, setMsg] = useState('');
  const wrapRef = useRef<HTMLDivElement>(null); // menu wrapper, for outside-click detection

  // Start/Stop disabled states. Both buttons stay live until the info
  // endpoint answers; an OTA pause locks both.
  const paused = dash.info?.paused;
  const running = dash.info?.running;
  const reason = dash.info?.pause_reason;
  // See "Two ways out of an overheat halt" in docs/MINING.md. An
  // overheat stop does not answer to a bare resume, so Start opens
  // the confirm step below and the confirm clears the halt and starts.
  const overheated = reason === 'dangerousTemperature';
  // Start is inert against an unreadable-sensor stop, and the same
  // section says why this menu offers no control for it.
  const unreadable = reason === 'temperatureUnreadable';
  // Whether Start is inert, which depends on why mining is stopped.
  // Start answers alone only where `userRequest` is the dominant
  // reason. `dangerousTemperature` is the exception: it is the highest
  // reason, so it can stand in front of a fan or a pool fault, and
  // Start stays live to open the confirm step. Every other reason
  // leaves it disabled. The severity order both tests rest on is in
  // the MINING.md section named above.
  function startIsDisabled(): boolean | undefined {
    if (paused == null) return false;
    if (!paused) return running;
    return reason !== 'userRequest' && !overheated;
  }
  const startDisabled = startIsDisabled();
  // Stop does not consult running: it asserts the operator pause, which
  // also interrupts bring-up and the automatic retries before mining starts.
  const stopDisabled = paused ?? false;

  function close() {
    setOpen(false);
    setConfirming(null);
    setMsg('');
  }

  function toggle() {
    open ? close() : setOpen(true);
  }

  useEffect(() => {
    // Close when a click lands outside the menu wrapper. The toggle
    // button and menu items live inside wrapRef, so their clicks never
    // trip this.
    function onWindowClick(event: MouseEvent) {
      // `contains` wants a Node; a click target is always one in
      // practice (it comes from the DOM event dispatch), just typed
      // more loosely as EventTarget.
      if (open && wrapRef.current && !wrapRef.current.contains(event.target as Node)) close();
    }
    function onKey(event: KeyboardEvent) {
      if (event.key === 'Escape' && open) close();
    }
    addEventListener('click', onWindowClick);
    addEventListener('keydown', onKey);
    return () => {
      removeEventListener('click', onWindowClick);
      removeEventListener('keydown', onKey);
    };
  }, [open]);

  async function act(path: string, label: string, onConflict?: () => void) {
    if (inFlight) return;
    setInFlight(true);
    setMsg('');
    try {
      await postJSON(path, {});
      close();
    } catch (error) {
      // A 409 means the board's state moved between the last poll and
      // this click. The action needs a step this menu already has, so
      // open it instead of reporting a failure the operator can do
      // nothing with. Every other failure gets this menu's own words:
      // the API message is a fragment and a dropped connection yields
      // the browser's own wording, and neither reads as operator text.
      if (onConflict && (error as ApiError).status === 409) {
        onConflict();
        return;
      }
      setMsg(`${label} failed. Try again.`);
    } finally {
      setInFlight(false);
    }
  }

  async function confirmRestart() {
    if (inFlight) return;
    setInFlight(true);
    setMsg('');
    try {
      await postJSON('/api/system/restart', {});
      close();
      boardNotice(
        'Restarting',
        'The board is rebooting. It can take up to 30 seconds to become reachable, then log in again.',
        true
      );
    } catch (_) {
      setMsg('Could not reach the board to restart. Try again.');
    } finally {
      setInFlight(false);
    }
  }

  return (
    <div class="actWrap" ref={wrapRef}>
      <button
        type="button"
        class="navBtn actBtn"
        aria-haspopup="true"
        aria-expanded={open}
        aria-label="Miner action menu"
        title="Miner actions"
        onClick={toggle}
      >
        <svg class="navIcon" aria-hidden="true">
          <use href="#iAction" />
        </svg>
      </button>

      <div class={'actMenu' + (open ? '' : ' hide')}>
        <button
          type="button"
          class={'actItem' + (confirming ? ' hide' : '')}
          disabled={startDisabled}
          onClick={() => {
            if (!overheated) {
              act('/api/system/resume', 'Start', () => setConfirming('thermal'));
              return;
            }
            setConfirming('thermal');
            setMsg('');
          }}
        >
          <svg class="navIcon" aria-hidden="true">
            <use href="#iPlay" />
          </svg>
          <span>Start</span>
        </button>
        <button
          type="button"
          class={'actItem' + (confirming ? ' hide' : '')}
          disabled={stopDisabled}
          onClick={() => act('/api/system/pause', 'Stop')}
        >
          <svg class="navIcon" aria-hidden="true">
            <use href="#iPause" />
          </svg>
          <span>Stop</span>
        </button>
        {unreadable && (
          <p class={'actMsg' + (confirming ? ' hide' : '')} role="status">
            No temperature reading. This stop clears by itself when the sensor answers again or
            when the board has nothing powered left to read.
          </p>
        )}
        <button
          type="button"
          class={'actItem' + (confirming ? ' hide' : '')}
          onClick={() => {
            setConfirming('restart');
            setMsg('');
          }}
        >
          <svg class="navIcon" aria-hidden="true">
            <use href="#iRestart" />
          </svg>
          <span>Restart…</span>
        </button>

        <div class={'actConfirm' + (confirming === 'thermal' ? '' : ' hide')}>
          <p class="actConfirmText">
            {dash.info?.chip_reading_stale
              ? 'The chip temperature stopped when the board stopped. It cannot show if the chips are cool now.' +
                (dash.info?.ambient != null
                  ? ' The ambient temperature still updates, but it shows the air around the board and not the chips.'
                  : '')
              : 'The chips still report the temperature that tripped this stop.'}{' '}
            Check the fans and the airflow by hand before clearing it. Mining starts again unless
            another problem still stops it. If the chips are still hot, it stops again.
          </p>
          <button
            type="button"
            class="actItem btnDanger"
            onClick={() => act('/api/system/thermal/clear', 'Clear overheat stop')}
          >
            Clear and start mining
          </button>
          <button type="button" class="actItem btnSecondary" onClick={() => setConfirming(null)}>
            Cancel
          </button>
        </div>

        <div class={'actConfirm' + (confirming === 'restart' ? '' : ' hide')}>
          <p class="actConfirmText">
            Restarts the entire board. Mining stops until the board comes back (~30 s).
          </p>
          <button type="button" class="actItem btnDanger" onClick={confirmRestart}>
            Confirm restart
          </button>
          <button type="button" class="actItem btnSecondary" onClick={() => setConfirming(null)}>
            Cancel
          </button>
        </div>

        {msg && (
          <p class="actMsg" role="status" aria-live="polite">
            {msg}
          </p>
        )}
      </div>
    </div>
  );
}
