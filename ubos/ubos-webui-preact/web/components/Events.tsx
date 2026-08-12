import { useEffect, useRef, useState } from 'preact/hooks';
import { getJSON } from '../lib/api';
import { collectSnapshot } from '../lib/snapshot';
import type { EventCatalogEntry, EventEntry, EventSeverity, EventsResponse } from '../lib/types';

// A catalog entry's format string plus the widths of the `{}` values
// packed into an event's hex-encoded `args`.
interface EventDef {
  fmt: string;
  args: string[];
}

// A raw EventEntry decorated with its rendered message and CSS severity
// class, computed once at decode time so sorting/filtering stays cheap.
interface DecoratedEvent extends EventEntry {
  message: string;
  level: EventSeverity;
}

type SortCol = 'time' | 'severity';
type SortDir = 'asc' | 'desc';

const SEVERITIES: EventSeverity[] = ['info', 'warn', 'error'];

function decodeArgs(hex: string, widths: string[]): number[] {
  let offset = 0;
  const values: number[] = [];
  for (const width of widths) {
    const byteCount = width.endsWith('8') ? 1 : width.endsWith('16') ? 2 : 4;
    let value = 0;
    for (let i = 0; i < byteCount; i++) {
      value |= parseInt(hex.slice((offset + i) * 2, (offset + i) * 2 + 2), 16) << (i * 8);
    }
    value = value >>> 0;
    if (width.startsWith('i')) {
      const bits = byteCount * 8;
      if (value >= 2 ** (bits - 1)) value -= 2 ** bits;
    }
    values.push(value);
    offset += byteCount;
  }
  return values;
}

function formatMessage(fmt: string, values: number[]): string {
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

function severityClass(sev: EventSeverity): EventSeverity {
  return sev === 'warn' ? 'warn' : sev === 'error' ? 'error' : 'info';
}

const SEV_RANK: Record<EventSeverity, number> = { error: 2, warn: 1, info: 0 };

export default function Events() {
  const [records, setRecords] = useState<DecoratedEvent[]>([]); // newest-first, decoded
  const defsRef = useRef<Map<number, EventDef> | null>(null);
  const [dropped, setDropped] = useState(false);
  const [statusText, setStatusText] = useState('');
  const [statusError, setStatusError] = useState(false);
  const [loading, setLoading] = useState(false);
  const [filterSet, setFilterSet] = useState<Set<EventSeverity>>(new Set(SEVERITIES));
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
    const level = severityClass(record.sev);
    return { ...record, message, level };
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
      setStatusError(false);
      setStatusText(next.length + ' events' + (nowDropped ? ' · older events dropped' : ''));
    } catch (error) {
      setStatusError(true);
      // `catch` binds `unknown` in strict mode; `error` is either a
      // fetch/JSON failure (an Error) or, rarely, something else, so
      // this mirrors lib/snapshot.ts's cast rather than trusting `.message`.
      setStatusText('Load failed: ' + ((error && (error as Error).message) || 'unknown'));
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

  const filtered = filterSet.size === 3 ? records : records.filter((r) => filterSet.has(r.sev));
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

  function toggleFilter(level: EventSeverity): void {
    const next = new Set(filterSet);
    if (next.has(level)) {
      if (next.size > 1) next.delete(level);
    } else {
      next.add(level);
    }
    setFilterSet(next);
  }

  const filterLabel =
    visible.length + ' events' + (filterSet.size < 3 ? ' (filtered)' : '') + (dropped ? ' · older events dropped' : '');

  return (
    <section class="events">
      <div class="evhead">
        <span id="evStatus" style={{ color: statusError ? 'var(--red)' : '' }}>
          {statusError ? statusText : filterLabel}
        </span>

        <div class="seg" style={{ marginLeft: 'auto' }}>
          {SEVERITIES.map((level) => (
            <button
              type="button"
              key={level}
              class={filterSet.has(level) ? 'active' : ''}
              onClick={() => toggleFilter(level)}
            >
              <span class={'sev-dot ' + level} />
              {level.charAt(0).toUpperCase() + level.slice(1)}
            </button>
          ))}
        </div>

        <button
          type="button"
          id="evReload"
          class={'navBtn' + (loading ? ' loading' : '')}
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
          class="navBtn"
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
            <tr class={'sev-' + record.level} key={record.seq}>
              <td>{formatTime(record.ts)}</td>
              <td>
                <span class={'badge ' + record.level}>{record.sev.toUpperCase()}</span>
              </td>
              <td>{record.message}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </section>
  );
}
