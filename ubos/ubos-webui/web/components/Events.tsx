import { useEffect, useRef, useState } from 'preact/hooks';
import { getJSON } from '../lib/api';
import { toast, errMsg } from '../lib/toast';
import { collectSnapshot } from '../lib/snapshot';
import type { EventCatalogEntry, EventEntry, EventSeverity, EventsResponse } from '../lib/types';

// A catalog entry's format string plus the widths of the `{}` values
// packed into an event's hex-encoded `args`.
interface EventDef {
  fmt: string;
  args: string[];
}

// A raw EventEntry decorated with its rendered message, computed once at
// decode time so sorting/filtering stays cheap.
interface DecoratedEvent extends EventEntry {
  message: string;
}

type SortCol = 'time' | 'severity';
type SortDir = 'asc' | 'desc';

const SEVERITIES: EventSeverity[] = ['info', 'warn', 'error'];

// Widths of the units that are held in thousandths of the unit shown:
// kHz as MHz, mV as volts, mW as watts, mJ/TH as J/TH.
const UNIT_WIDTHS = ['frequency', 'voltage', 'power', 'efficiency'];

function decodeArgs(hex: string, widths: string[]): (number | string)[] {
  let offset = 0;
  const values: (number | string)[] = [];
  for (const width of widths) {
    const byteCount = width.endsWith('8') ? 1 : width.endsWith('16') ? 2 : 4;
    let value = 0;
    for (let i = 0; i < byteCount; i++) {
      value |= parseInt(hex.slice((offset + i) * 2, (offset + i) * 2 + 2), 16) << (i * 8);
    }
    value = value >>> 0;
    const temperature = width === 'temperature';
    if (temperature || width.startsWith('i')) {
      const bits = byteCount * 8;
      if (value >= 2 ** (bits - 1)) value -= 2 ** bits;
    }
    // A unit travels in thousandths of the unit the message shows, which
    // the log line prints with three decimals.
    values.push(temperature || UNIT_WIDTHS.includes(width) ? (value / 1000).toFixed(3) : value);
    offset += byteCount;
  }
  return values;
}

function formatMessage(fmt: string, values: (number | string)[]): string {
  let i = 0;
  return fmt.replace(/\{\}/g, () => {
    const value = values[i];
    i += 1;
    return value != null ? String(value) : '';
  });
}

function formatTime(ts: number): string {
  return ts >= 1000000000 ? new Date(ts * 1000).toLocaleString() : ts + 's (pre-sync)';
}

const SEV_RANK: Record<EventSeverity, number> = { error: 2, warn: 1, info: 0 };

export default function Events() {
  const [records, setRecords] = useState<DecoratedEvent[]>([]); // newest-first, decoded
  const defsRef = useRef<Map<number, EventDef> | null>(null);
  const [dropped, setDropped] = useState(false);
  const [loading, setLoading] = useState(false);
  const [filter, setFilter] = useState<'all' | EventSeverity>('all');
  const [sortCol, setSortCol] = useState<SortCol>('time');
  const [sortDir, setSortDir] = useState<SortDir>('desc');
  const recordsRef = useRef(records);
  recordsRef.current = records;
  const loadingRef = useRef(loading);
  loadingRef.current = loading;
  const droppedRef = useRef(dropped);
  droppedRef.current = dropped;

  async function loadDefs(): Promise<void> {
    if (defsRef.current) return;
    const list = await getJSON<EventCatalogEntry[]>('/api/system/events/catalog');
    const defs = new Map<number, EventDef>();
    for (const entry of list) defs.set(entry.code, { fmt: entry.fmt, args: entry.args });
    defsRef.current = defs;
  }

  function decorate(record: EventEntry): DecoratedEvent {
    const def = defsRef.current ? defsRef.current.get(record.code) : null;
    const message = def ? formatMessage(def.fmt, decodeArgs(record.args, def.args)) : 'event ' + record.code;
    return { ...record, message };
  }

  async function load(incremental: boolean): Promise<void> {
    if (loadingRef.current) return;
    setLoading(true);
    let nowDropped = droppedRef.current;
    if (!incremental) nowDropped = false;
    try {
      await loadDefs();
      const current = recordsRef.current;
      const lastSeq = current.length > 0 ? current[0].seq : null;
      const url = '/api/system/events' + (incremental && lastSeq != null ? '?since=' + lastSeq : '');
      const data = await getJSON<EventsResponse>(url);
      const incoming = data.events || [];
      if (incremental && lastSeq != null && incoming.length > 0 && incoming[0].seq > lastSeq + 1) {
        nowDropped = true;
      }
      const bySeq = new Map<number, DecoratedEvent>(incremental ? current.map((r) => [r.seq, r]) : []);
      for (const record of incoming) bySeq.set(record.seq, decorate(record));
      const next = Array.from(bySeq.values()).sort((a, b) => b.seq - a.seq);
      setRecords(next);
      setDropped(nowDropped);
    } catch (error) {
      // A user-driven load (mount or the reload button, both
      // incremental=false) reports the failure; the 60 s background
      // poll fails silently so an unreachable board does not stack
      // persistent toasts.
      if (!incremental) toast.err('Could not load events', errMsg(error));
    } finally {
      setLoading(false);
    }
  }

  useEffect(() => {
    load(false);
    const timer = setInterval(() => load(true), 60000);
    return () => clearInterval(timer);
  }, []);

  // Download a diagnostic snapshot of every read-only endpoint as JSON.
  const [saving, setSaving] = useState(false);
  async function saveSnapshot(): Promise<void> {
    if (saving) return;
    setSaving(true);
    try {
      const snapshot = await collectSnapshot();
      // collectSnapshot's return type is a generic Record<string, unknown>
      // since its sections are heterogeneous API payloads; `meta` is known
      // from that module's own construction, so this cast is a local,
      // unavoidable narrowing rather than trusting an arbitrary shape.
      const meta = snapshot.meta as { generated_at: string };
      const stamp = meta.generated_at.slice(0, 19).replace(/:/g, '-') + 'Z';
      const blob = new Blob([JSON.stringify(snapshot, null, 2)], { type: 'application/json' });
      const url = URL.createObjectURL(blob);
      const link = document.createElement('a');
      link.href = url;
      link.download = 'ubos-snapshot-' + location.hostname + '-' + stamp + '.json';
      link.click();
      // Revoking synchronously can race the browser starting the
      // download, so free it after a delay.
      setTimeout(() => URL.revokeObjectURL(url), 60000);
    } finally {
      setSaving(false);
    }
  }

  function toggleSort(col: SortCol): void {
    if (sortCol === col) {
      setSortDir(sortDir === 'desc' ? 'asc' : 'desc');
    } else {
      setSortCol(col);
      setSortDir('desc');
    }
  }

  const counts: Record<EventSeverity, number> = { info: 0, warn: 0, error: 0 };
  for (const r of records) counts[r.sev] += 1;

  const filtered = filter === 'all' ? records : records.filter((r) => r.sev === filter);
  const sign = sortDir === 'desc' ? 1 : -1;
  const visible = filtered.slice().sort((a, b) => {
    if (sortCol === 'severity') {
      const d = ((SEV_RANK[b.sev] || 0) - (SEV_RANK[a.sev] || 0)) * sign;
      return d !== 0 ? d : b.ts - a.ts;
    }
    const d = (b.ts - a.ts) * sign;
    return d !== 0 ? d : (b.seq - a.seq) * sign;
  });

  function thClass(col: SortCol): string {
    let cls = 'sortable';
    if (sortCol === col) cls += ' sorted ' + sortDir;
    return cls;
  }

  return (
    <section class="events">
      <div class="evhead">
        <h1 class="evTitle">Events</h1>
        <div class="evActions">
          <button
            type="button"
            id="evReload"
            class={'navBtn evIcon' + (loading ? ' loading' : '')}
            aria-label="Reload events"
            title="Reload"
            onClick={() => load(false)}
          >
            <svg class="evReloadIcon" aria-hidden="true">
              <use href="#iRenew" />
            </svg>
          </button>
          <button
            type="button"
            id="evSave"
            class="navBtn evIcon"
            aria-label="Download snapshot"
            title="Download snapshot"
            disabled={saving}
            onClick={saveSnapshot}
          >
            <svg class="evReloadIcon" aria-hidden="true">
              <use href="#iSave" />
            </svg>
          </button>
        </div>
      </div>

      <nav class="evTabs">
        <button
          type="button"
          class={'evTab' + (filter === 'all' ? ' active' : '')}
          onClick={() => setFilter('all')}
        >
          All <span class="evCount">{records.length}</span>
        </button>
        {SEVERITIES.map((level) => (
          <button
            type="button"
            key={level}
            class={'evTab' + (filter === level ? ' active' : '')}
            onClick={() => setFilter(level)}
          >
            {level.charAt(0).toUpperCase() + level.slice(1)}{' '}
            <span class="evCount">{counts[level]}</span>
          </button>
        ))}
      </nav>

      <table class="evtab">
        <thead>
          <tr>
            <th class={thClass('time')} onClick={() => toggleSort('time')}>
              Time
              <span class="sort-arrow" />
            </th>
            <th class={thClass('severity')} onClick={() => toggleSort('severity')}>
              Severity
              <span class="sort-arrow" />
            </th>
            <th>Message</th>
          </tr>
        </thead>
        <tbody>
          {visible.map((record) => (
            <tr key={record.seq}>
              <td>{formatTime(record.ts)}</td>
              <td>
                <span class={'badge ' + record.sev}>{record.sev.toUpperCase()}</span>
              </td>
              <td>{record.message}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </section>
  );
}
